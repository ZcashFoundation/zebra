use super::{config::*, events::*, pipe::*, wire::*, *};
use crate::zakura::{
    handle_pipe_exit, spawn_supervised_pipe, Flow, FramedSend, OrderedSendError, Peer,
    PeerStreamSession, Pipe, Service, SinkReject, Stream, StreamMode, ZakuraPeerId,
    FRAME_HEADER_BYTES,
};
use std::sync::atomic::{AtomicU64, Ordering};
// The per-peer block-sync `Source` frame-pump is test-only scaffolding (see
// `BlockSyncPeerRecord` / `add_peer`); its trait and boxed-future alias are only
// referenced by that `cfg(test)` task.
#[cfg(test)]
use crate::zakura::{BoxRunFuture, Source};

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
    events: mpsc::Sender<BlockSyncEvent>,
    lifecycle: mpsc::UnboundedSender<BlockSyncEvent>,
    peers: StdMutex<HashMap<ZakuraPeerId, BlockSyncPeerRecord>>,
    next_session_id: AtomicU64,
}

#[derive(Debug)]
struct BlockSyncPeerRecord {
    session_id: u64,
    direction: ServicePeerDirection,
    cancel_token: CancellationToken,
    // Production outbound block-sync sends are authoritative through
    // `BlockSyncPeerSession`: the reactor calls `try_send_get_blocks`/etc.
    // directly (see `reactor::schedule`). The per-peer `BlockSyncSource` action
    // pump and its `actions` sender are test-only scaffolding — no non-test code
    // produces into this channel, and `drive_block_sync_actions` deliberately
    // ignores the reactor's duplicate `SendMessage` to avoid double-sending.
    // Gating the sender and the task handle to `cfg(test)` keeps that contract
    // compiler-enforced: production has no producer to wire and therefore cannot
    // double-send, and it retains no idle frame-pump task/channel per peer.
    #[cfg(test)]
    actions: mpsc::Sender<BlockSyncAction>,
    #[cfg(test)]
    _tasks: Vec<JoinHandle<()>>,
}

impl BlockSyncService {
    pub(crate) fn new(config: ZakuraBlockSyncConfig) -> Self {
        Self::new_with_startup(BlockSyncStartup::inert(config))
    }

    pub(crate) fn new_with_handle(config: ZakuraBlockSyncConfig, handle: BlockSyncHandle) -> Self {
        Self {
            inner: Arc::new(BlockSyncServiceInner {
                config,
                events: handle.events.clone(),
                lifecycle: handle.lifecycle.clone(),
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
                events: handle.events.clone(),
                lifecycle: handle.lifecycle.clone(),
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
                    events,
                    lifecycle,
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

    #[cfg(test)]
    pub(crate) async fn send_action(
        &self,
        action: BlockSyncAction,
    ) -> Result<(), mpsc::error::SendError<BlockSyncAction>> {
        let BlockSyncAction::SendMessage { peer, .. } = &action else {
            return Err(mpsc::error::SendError(action));
        };
        let peer = peer.clone();
        let sender = match self.inner.peers.lock() {
            Ok(peers) => peers.get(&peer).map(|record| record.actions.clone()),
            Err(_) => return Err(mpsc::error::SendError(action)),
        };
        let Some(sender) = sender else {
            return Err(mpsc::error::SendError(action));
        };
        sender.send(action).await
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
        if !self.peer_slots_free(peer.direction) {
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

        // The per-peer block-sync source frame-pump is test-only scaffolding (see
        // `BlockSyncPeerRecord`). Production outbound frames go directly through
        // `BlockSyncPeerSession`, so only the test build spawns the source to
        // exercise `send_action`; production drops the redundant transport sender.
        // The outbound stream stays alive through the `BlockSyncPeerSession` clone
        // the reactor holds, so nothing is lost by not retaining it here.
        #[cfg(test)]
        let (actions_tx, source_task) = {
            let (actions_tx, actions_rx) =
                mpsc::channel(self.inner.config.peer_limits.outbound_queue_depth.max(1));
            let source_task = spawn_block_sync_source(
                peer_id.clone(),
                actions_rx,
                service_cancel_token.clone(),
                send,
            );
            (actions_tx, source_task)
        };
        #[cfg(not(test))]
        drop(send);

        let events = self.inner.events.clone();
        let run_peer_id = peer_id.clone();
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
        // A protocol reject is fatal to the whole connection; a normal/parked exit
        // leaves it for the other services riding on it. Panic teardown is in
        // `on_panic`.
        let pipe = async move {
            handle_pipe_exit(
                "block-sync",
                &connection_cancel_token,
                run_peer(run_peer_id, recv, events, run_cancel).await,
            );
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
                    #[cfg(test)]
                    actions: actions_tx,
                    #[cfg(test)]
                    _tasks: vec![source_task],
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
        peer_id: ZakuraPeerId,
        stream_kind: u16,
        frame: Frame,
    ) -> Result<(), SinkReject> {
        if stream_kind != ZAKURA_STREAM_BLOCK_SYNC {
            return Ok(());
        }

        let mut pipe = block_sync_pipe(peer_id, self.inner.events.clone());
        match pipe.run_one(frame) {
            Flow::Continue(()) | Flow::Done => Ok(()),
            Flow::Reject(reject) => Err(reject),
        }
    }
}

// Test-only per-peer outbound frame-pump. Production block-sync sends go through
// `BlockSyncPeerSession` directly (see `add_peer` / `BlockSyncPeerRecord`); this
// source exists solely so tests can drive outbound frames via `send_action`.
#[cfg(test)]
fn spawn_block_sync_source(
    peer_id: ZakuraPeerId,
    actions: mpsc::Receiver<BlockSyncAction>,
    cancel_token: CancellationToken,
    send: FramedSend,
) -> JoinHandle<()> {
    tokio::task::spawn(async move {
        let source = Box::new(BlockSyncSource {
            peer_id,
            actions,
            cancel_token,
        });
        source.run(send).await;
    })
}

#[cfg(test)]
#[derive(Debug)]
struct BlockSyncSource {
    peer_id: ZakuraPeerId,
    actions: mpsc::Receiver<BlockSyncAction>,
    cancel_token: CancellationToken,
}

#[cfg(test)]
impl Source for BlockSyncSource {
    fn run(mut self: Box<Self>, send: FramedSend) -> BoxRunFuture<'static, ()> {
        Box::pin(async move {
            loop {
                let action = tokio::select! {
                    _ = self.cancel_token.cancelled() => return,
                    action = self.actions.recv() => {
                        let Some(action) = action else {
                            return;
                        };
                        action
                    }
                };

                let BlockSyncAction::SendMessage { peer, msg } = action else {
                    continue;
                };
                if peer != self.peer_id {
                    continue;
                }

                let frame = match msg.encode_frame() {
                    Ok(frame) => frame,
                    Err(error) => {
                        tracing::debug!(
                            ?error,
                            peer = ?self.peer_id,
                            "block-sync source refused to encode outbound message"
                        );
                        continue;
                    }
                };

                if send.send(frame).await.is_err() {
                    return;
                }
            }
        })
    }
}

pub(super) fn block_sync_pipe(
    peer_id: ZakuraPeerId,
    events: mpsc::Sender<BlockSyncEvent>,
) -> Pipe<BsLocal, BsEnv> {
    Pipe::new(
        peer_id,
        BsLocal,
        BsEnv::new(events),
        block_sync_guard(),
        run_inbound,
        &PIPE_SHAPE,
    )
}
