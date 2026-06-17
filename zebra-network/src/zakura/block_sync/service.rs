use super::{config::*, events::*, wire::*, *};
use crate::zakura::{
    BoxRunFuture, FramedRecv, FramedSend, OrderedSendError, Peer, PeerStreamSession, Service, Sink,
    SinkReject, Source, Stream, StreamMode, ZakuraPeerId, FRAME_HEADER_BYTES,
};

/// Maximum frame bytes for one stream-6 body frame plus protocol framing.
///
/// A block body is one frame and may be up to Zebra's `MAX_BLOCK_BYTES`; the
/// stream also carries the inherited inner message discriminator and outer
/// Zakura frame header, so it is intentionally larger than control frames.
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
}

#[derive(Debug)]
struct BlockSyncPeerRecord {
    direction: ServicePeerDirection,
    cancel_token: CancellationToken,
    #[cfg(test)]
    actions: mpsc::Sender<BlockSyncAction>,
    #[cfg(not(test))]
    _actions: mpsc::Sender<BlockSyncAction>,
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
        let (_session_peer, _stream_kind, recv, send, _session_cancel) = session.into_parts();
        let (actions_tx, actions_rx) =
            mpsc::channel(self.inner.config.peer_limits.outbound_queue_depth.max(1));

        let source_task = spawn_block_sync_source(
            peer_id.clone(),
            actions_rx,
            service_cancel_token.clone(),
            send,
        );
        let sink_task = spawn_block_sync_sink(
            peer_id.clone(),
            recv,
            self.inner.events.clone(),
            self.inner.lifecycle.clone(),
            service_cancel_token.clone(),
            connection_cancel_token,
        );

        {
            let mut peers = self
                .inner
                .peers
                .lock()
                .expect("block-sync peer map mutex is never poisoned");
            peers.insert(
                peer_id.clone(),
                BlockSyncPeerRecord {
                    direction: peer.direction,
                    cancel_token: service_cancel_token,
                    #[cfg(test)]
                    actions: actions_tx.clone(),
                    #[cfg(not(test))]
                    _actions: actions_tx.clone(),
                    _tasks: vec![source_task, sink_task],
                },
            );
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

        deliver_block_sync_frame(&self.inner.events, peer_id, frame)
    }
}

fn spawn_block_sync_sink(
    peer_id: ZakuraPeerId,
    recv: FramedRecv,
    events: mpsc::Sender<BlockSyncEvent>,
    lifecycle: mpsc::UnboundedSender<BlockSyncEvent>,
    service_cancel_token: CancellationToken,
    connection_cancel_token: CancellationToken,
) -> JoinHandle<()> {
    task::spawn(async move {
        let sink = Box::new(BlockSyncSink {
            peer_id: peer_id.clone(),
            events: events.clone(),
            cancel_token: service_cancel_token,
        });

        match sink.run(recv).await {
            Ok(()) => {}
            Err(SinkReject::Protocol(error)) => {
                tracing::debug!(
                    ?error,
                    ?peer_id,
                    "block-sync stream rejected protocol-invalid frame"
                );
                connection_cancel_token.cancel();
            }
            Err(SinkReject::Local(error)) => {
                tracing::debug!(
                    ?error,
                    ?peer_id,
                    "block-sync stream could not deliver frame locally"
                );
            }
        }

        let _ = lifecycle.send(BlockSyncEvent::PeerDisconnected(peer_id));
    })
}

fn spawn_block_sync_source(
    peer_id: ZakuraPeerId,
    actions: mpsc::Receiver<BlockSyncAction>,
    cancel_token: CancellationToken,
    send: FramedSend,
) -> JoinHandle<()> {
    task::spawn(async move {
        let source = Box::new(BlockSyncSource {
            peer_id,
            actions,
            cancel_token,
        });
        source.run(send).await;
    })
}

#[derive(Debug)]
struct BlockSyncSink {
    peer_id: ZakuraPeerId,
    events: mpsc::Sender<BlockSyncEvent>,
    cancel_token: CancellationToken,
}

impl Sink for BlockSyncSink {
    fn run(self: Box<Self>, mut recv: FramedRecv) -> BoxRunFuture<'static, Result<(), SinkReject>> {
        Box::pin(async move {
            loop {
                let frame = tokio::select! {
                    _ = self.cancel_token.cancelled() => return Ok(()),
                    frame = recv.recv() => {
                        let Some(frame) = frame else {
                            return Ok(());
                        };
                        frame
                    }
                };

                deliver_block_sync_frame(&self.events, self.peer_id.clone(), frame)?;
            }
        })
    }
}

#[derive(Debug)]
struct BlockSyncSource {
    peer_id: ZakuraPeerId,
    actions: mpsc::Receiver<BlockSyncAction>,
    cancel_token: CancellationToken,
}

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

fn deliver_block_sync_frame(
    events: &mpsc::Sender<BlockSyncEvent>,
    peer_id: ZakuraPeerId,
    frame: Frame,
) -> Result<(), SinkReject> {
    let msg = match BlockSyncMessage::decode_frame(frame) {
        Ok(msg) => msg,
        Err(error) => {
            // Block bodies are validated against committed headers in B1+.
            let protocol_error =
                std::io::Error::new(std::io::ErrorKind::InvalidData, error.to_string());
            let _ = events.try_send(BlockSyncEvent::WireDecodeFailed {
                peer: peer_id,
                error: Arc::new(error),
            });
            return Err(SinkReject::protocol(protocol_error));
        }
    };

    events
        .try_send(BlockSyncEvent::WireMessage { peer: peer_id, msg })
        .map_err(|error| SinkReject::local(format!("block-sync queue closed: {error}")))
}
