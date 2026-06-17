use std::{
    collections::HashMap,
    sync::{Arc, Mutex as StdMutex, OnceLock},
};

use tokio::{sync::mpsc, task};
use tokio_util::sync::CancellationToken;

use super::{events::*, validation::*, wire::*, *};
use crate::{
    zakura::{
        BoxRunFuture, Frame, FramedRecv, FramedSend, Peer, Service, Sink, SinkReject, Stream,
        StreamMode, ZakuraPeerId, ZakuraSupervisorHandle, ZAKURA_CAP_HEADER_SYNC,
    },
    BoxError,
};

const HEADER_SYNC_SERVICE_STREAMS: [Stream; 1] = [Stream {
    kind: ZAKURA_STREAM_HEADER_SYNC,
    version: ZAKURA_HEADER_SYNC_STREAM_VERSION,
    // Advisory until the transport wires Stream::frame_cap end-to-end; the
    // authoritative inbound cap is app_frame_cap_for_stream_kind. The cast is
    // safe because both terms are small protocol constants checked against the
    // local message cap in header_sync::wire.
    frame_cap: (MAX_HS_MESSAGE_BYTES + FRAME_HEADER_BYTES) as u32,
    capability: ZAKURA_CAP_HEADER_SYNC,
    mode: StreamMode::Ordered,
}];

/// Service-declared streams for native header sync.
pub(crate) fn header_sync_streams() -> &'static [Stream] {
    &HEADER_SYNC_SERVICE_STREAMS
}

/// Stream-5 outbound sources keyed by peer.
#[derive(Clone, Debug, Default)]
pub(crate) struct HeaderSyncOutbound {
    senders: Arc<StdMutex<HashMap<ZakuraPeerId, FramedSend>>>,
}

impl HeaderSyncOutbound {
    pub(crate) async fn send(&self, peer: &ZakuraPeerId, frame: Frame) -> Result<(), BoxError> {
        let sender = {
            let senders = self
                .senders
                .lock()
                .expect("header-sync outbound mutex is never poisoned");
            senders.get(peer).cloned()
        };
        let Some(sender) = sender else {
            return Err("no ready Zakura header-sync source for peer".into());
        };

        sender
            .send(frame)
            .await
            .map_err(|_| -> BoxError { "Zakura header-sync source queue closed".into() })
    }

    fn insert(&self, peer: ZakuraPeerId, sender: FramedSend) {
        self.senders
            .lock()
            .expect("header-sync outbound mutex is never poisoned")
            .insert(peer, sender);
    }

    fn remove(&self, peer: &ZakuraPeerId) {
        self.senders
            .lock()
            .expect("header-sync outbound mutex is never poisoned")
            .remove(peer);
    }
}

static HEADER_SYNC_OUTBOUND_BY_SUPERVISOR: OnceLock<StdMutex<HashMap<u64, HeaderSyncOutbound>>> =
    OnceLock::new();

pub(crate) fn header_sync_outbound_for_supervisor(
    supervisor: &ZakuraSupervisorHandle,
) -> HeaderSyncOutbound {
    let registry = HEADER_SYNC_OUTBOUND_BY_SUPERVISOR.get_or_init(Default::default);
    let mut registry = registry
        .lock()
        .expect("header-sync outbound registry mutex is never poisoned");
    registry.entry(supervisor.id()).or_default().clone()
}

/// Send a stream-5 message through the service-owned source for `peer`.
pub(crate) async fn send_header_sync_message(
    supervisor: &ZakuraSupervisorHandle,
    peer: &ZakuraPeerId,
    msg: HeaderSyncMessage,
) {
    let frame = match msg.encode_frame() {
        Ok(frame) => frame,
        Err(error) => {
            tracing::debug!(?error, "failed to encode Zakura header-sync frame");
            return;
        }
    };

    let outbound = header_sync_outbound_for_supervisor(supervisor);
    if let Err(error) = outbound.send(peer, frame).await {
        tracing::debug!(?error, ?peer, "failed to send Zakura header-sync frame");
    }
}

/// Pump actor actions that can be satisfied at the transport/service seam.
pub(crate) async fn drive_header_sync_actions(
    mut actions: mpsc::Receiver<HeaderSyncAction>,
    handle: HeaderSyncHandle,
    supervisor: ZakuraSupervisorHandle,
    shutdown: CancellationToken,
) {
    loop {
        let action = tokio::select! {
            _ = shutdown.cancelled() => return,
            action = actions.recv() => {
                let Some(action) = action else {
                    return;
                };
                action
            }
        };

        match action {
            HeaderSyncAction::SendMessage { peer, msg } => {
                send_header_sync_message(&supervisor, &peer, msg).await;
            }
            HeaderSyncAction::ForwardNewBlock { peer, block, .. } => {
                send_header_sync_message(&supervisor, &peer, HeaderSyncMessage::NewBlock(block))
                    .await;
            }
            HeaderSyncAction::Misbehavior { peer, reason } => {
                tracing::debug!(
                    ?peer,
                    ?reason,
                    "disconnecting peer for Zakura header-sync violation"
                );
                let _ = supervisor.disconnect_peer(&peer).await;
            }
            HeaderSyncAction::NewBlockReceived {
                peer,
                hash,
                block: _,
                ..
            } => {
                tracing::debug!(
                    ?peer,
                    ?hash,
                    "Zakura header-sync NewBlock body arrived before block-acceptance hook is wired"
                );
            }
            HeaderSyncAction::QueryHeadersByHeightRange { peer, start, count } => {
                let _ = handle
                    .send(HeaderSyncEvent::HeaderRangeResponseFinished {
                        peer,
                        start_height: start,
                        requested_count: count,
                        returned_count: 0,
                    })
                    .await;
            }
            HeaderSyncAction::CommitHeaderRange {
                peer,
                start_height,
                headers,
                ..
            } => {
                tracing::debug!(
                    ?peer,
                    ?start_height,
                    count = headers.len(),
                    "suppressing Zakura header range commit until state driver is wired"
                );
            }
            HeaderSyncAction::QueryBestHeaderTip
            | HeaderSyncAction::QueryMissingBlockBodies { .. }
            | HeaderSyncAction::BodyGaps { .. } => {}
        }
    }
}

/// Native stream-5 header-sync service.
#[derive(Debug)]
pub(crate) struct HeaderSyncService {
    header_sync: HeaderSyncHandle,
    outbound: HeaderSyncOutbound,
}

impl HeaderSyncService {
    pub(crate) fn new(header_sync: HeaderSyncHandle, outbound: HeaderSyncOutbound) -> Self {
        Self {
            header_sync,
            outbound,
        }
    }
}

impl Service for HeaderSyncService {
    fn name(&self) -> &'static str {
        "header-sync"
    }

    fn streams(&self) -> &[Stream] {
        header_sync_streams()
    }

    fn add_peer(&self, mut peer: Peer) {
        let Some((recv, send)) = peer.take_stream(ZAKURA_STREAM_HEADER_SYNC) else {
            return;
        };

        let peer_id = peer.id.clone();
        let cancel_token = peer.cancel_token();

        self.outbound.insert(peer_id.clone(), send);
        let _ = self
            .header_sync
            .send_lifecycle(HeaderSyncEvent::PeerConnected(peer_id.clone()));

        spawn_header_sync_sink(
            peer_id,
            recv,
            self.header_sync.clone(),
            self.outbound.clone(),
            cancel_token,
        );
    }

    fn remove_peer(&self, peer: &ZakuraPeerId) {
        self.outbound.remove(peer);
        let _ = self
            .header_sync
            .send_lifecycle(HeaderSyncEvent::PeerDisconnected(peer.clone()));
    }

    fn deliver_frame(
        &self,
        peer_id: ZakuraPeerId,
        stream_kind: u16,
        frame: Frame,
    ) -> Result<(), SinkReject> {
        if stream_kind != ZAKURA_STREAM_HEADER_SYNC {
            return Ok(());
        }

        deliver_header_sync_frame(&self.header_sync, peer_id, frame)
    }

    fn request_frame<'a>(
        &'a self,
        _peer_id: ZakuraPeerId,
        _stream_kind: u16,
        _request_id: u64,
        _max_frame_bytes: u32,
        _frame: Frame,
    ) -> BoxRunFuture<'a, Result<Vec<Frame>, SinkReject>> {
        Box::pin(async {
            Err(SinkReject::protocol(
                "header-sync request streams are not supported",
            ))
        })
    }
}

/// Testkit/no-reactor mode records stream-5 inbound frames without running header sync.
#[derive(Debug)]
pub(crate) struct HeaderSyncPassthroughService {
    inner: Arc<dyn Service>,
}

impl HeaderSyncPassthroughService {
    pub(crate) fn new(inner: Arc<dyn Service>) -> Self {
        Self { inner }
    }
}

impl Service for HeaderSyncPassthroughService {
    fn name(&self) -> &'static str {
        "header-sync-passthrough"
    }

    fn streams(&self) -> &[Stream] {
        header_sync_streams()
    }

    fn add_peer(&self, mut peer: Peer) {
        let Some((recv, _send)) = peer.take_stream(ZAKURA_STREAM_HEADER_SYNC) else {
            return;
        };

        let inner = self.inner.clone();
        let peer_id = peer.id.clone();
        let cancel_token = peer.cancel_token();

        task::spawn(async move {
            let sink = Box::new(HeaderSyncPassthroughSink {
                peer_id: peer_id.clone(),
                inner,
                cancel_token: cancel_token.clone(),
            });

            match sink.run(recv).await {
                Ok(()) => {}
                Err(SinkReject::Protocol(error)) => {
                    tracing::debug!(
                        ?error,
                        ?peer_id,
                        "header-sync passthrough rejected protocol-invalid frame"
                    );
                    cancel_token.cancel();
                }
                Err(SinkReject::Local(error)) => {
                    tracing::debug!(
                        ?error,
                        ?peer_id,
                        "header-sync passthrough could not deliver frame locally"
                    );
                }
            }
        });
    }

    fn remove_peer(&self, _peer: &ZakuraPeerId) {}

    fn deliver_frame(
        &self,
        peer_id: ZakuraPeerId,
        stream_kind: u16,
        frame: Frame,
    ) -> Result<(), SinkReject> {
        self.inner.deliver_frame(peer_id, stream_kind, frame)
    }

    fn request_frame<'a>(
        &'a self,
        _peer_id: ZakuraPeerId,
        _stream_kind: u16,
        _request_id: u64,
        _max_frame_bytes: u32,
        _frame: Frame,
    ) -> BoxRunFuture<'a, Result<Vec<Frame>, SinkReject>> {
        Box::pin(async {
            Err(SinkReject::protocol(
                "header-sync request streams are not supported",
            ))
        })
    }
}

fn spawn_header_sync_sink(
    peer_id: ZakuraPeerId,
    recv: FramedRecv,
    header_sync: HeaderSyncHandle,
    outbound: HeaderSyncOutbound,
    cancel_token: CancellationToken,
) {
    task::spawn(async move {
        let sink = Box::new(HeaderSyncSink {
            peer_id: peer_id.clone(),
            header_sync,
            cancel_token: cancel_token.clone(),
        });

        let result = sink.run(recv).await;
        outbound.remove(&peer_id);

        match result {
            Ok(()) => {}
            Err(SinkReject::Protocol(error)) => {
                tracing::debug!(
                    ?error,
                    ?peer_id,
                    "header-sync stream rejected protocol-invalid frame"
                );
                cancel_token.cancel();
            }
            Err(SinkReject::Local(error)) => {
                tracing::debug!(
                    ?error,
                    ?peer_id,
                    "header-sync stream could not deliver frame locally"
                );
            }
        }
    });
}

#[derive(Debug)]
struct HeaderSyncSink {
    peer_id: ZakuraPeerId,
    header_sync: HeaderSyncHandle,
    cancel_token: CancellationToken,
}

impl Sink for HeaderSyncSink {
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

                match deliver_header_sync_frame(&self.header_sync, self.peer_id.clone(), frame) {
                    Ok(()) => {}
                    Err(SinkReject::Protocol(error)) => return Err(SinkReject::Protocol(error)),
                    Err(SinkReject::Local(error)) => {
                        tracing::debug!(
                            ?error,
                            peer_id = ?self.peer_id,
                            "header-sync stream could not deliver frame locally"
                        );
                    }
                }
            }
        })
    }
}

#[derive(Debug)]
struct HeaderSyncPassthroughSink {
    peer_id: ZakuraPeerId,
    inner: Arc<dyn Service>,
    cancel_token: CancellationToken,
}

impl Sink for HeaderSyncPassthroughSink {
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

                match self.inner.deliver_frame(
                    self.peer_id.clone(),
                    ZAKURA_STREAM_HEADER_SYNC,
                    frame,
                ) {
                    Ok(()) => {}
                    Err(SinkReject::Protocol(error)) => return Err(SinkReject::Protocol(error)),
                    Err(SinkReject::Local(error)) => {
                        tracing::debug!(
                            ?error,
                            peer_id = ?self.peer_id,
                            "header-sync passthrough could not deliver frame locally"
                        );
                    }
                }
            }
        })
    }
}

fn deliver_header_sync_frame(
    header_sync: &HeaderSyncHandle,
    peer_id: ZakuraPeerId,
    frame: Frame,
) -> Result<(), SinkReject> {
    if u8::try_from(frame.message_type).ok() == Some(MSG_HS_HEADERS) {
        // `Headers` response decode still needs the actor's per-peer
        // outstanding-request state; the per-peer concurrency epic moves that
        // contract into this sink and removes the residual raw-frame hop.
        return header_sync
            .try_send(HeaderSyncEvent::WireFrame {
                peer: peer_id,
                frame,
            })
            .map_err(|error| SinkReject::local(format!("header-sync queue closed: {error}")));
    }

    let msg = match decode_header_sync_frame(frame) {
        Ok(msg) => msg,
        Err(error) => {
            let protocol_error =
                std::io::Error::new(std::io::ErrorKind::InvalidData, error.to_string());
            let _ = header_sync.try_send(HeaderSyncEvent::WireDecodeFailed {
                peer: peer_id,
                error: Arc::new(error),
            });
            return Err(SinkReject::protocol(protocol_error));
        }
    };

    header_sync
        .try_send(HeaderSyncEvent::WireMessage { peer: peer_id, msg })
        .map_err(|error| SinkReject::local(format!("header-sync queue closed: {error}")))
}

fn decode_header_sync_frame(frame: Frame) -> Result<HeaderSyncMessage, HeaderSyncWireError> {
    if u8::try_from(frame.message_type).ok() == Some(MSG_HS_HEADERS) {
        return Err(HeaderSyncWireError::UnsolicitedHeaders);
    }

    HeaderSyncMessage::decode_frame(frame, HeaderSyncDecodeContext::control())
}
