use std::{
    collections::VecDeque,
    sync::{Arc, Mutex as StdMutex},
};

use tokio::{sync::mpsc, task};
use tokio_util::sync::CancellationToken;

use super::{events::*, validation::*, wire::*, *};
use crate::zakura::{
    BoxRunFuture, Frame, FramedRecv, FramedSend, OrderedSendError, Peer, PeerStreamSession,
    Service, ServicePeerDirection, Sink, SinkReject, Stream, StreamMode, ZakuraPeerId,
    ZakuraSupervisorHandle, ZAKURA_CAP_HEADER_SYNC,
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

/// Cloneable typed stream-5 sender and peer-local response expectations.
#[derive(Clone, Debug)]
pub struct HeaderSyncPeerSession {
    peer_id: ZakuraPeerId,
    direction: ServicePeerDirection,
    inner: Arc<HeaderSyncPeerSessionInner>,
}

#[derive(Debug)]
struct HeaderSyncPeerSessionInner {
    send: FramedSend,
    cancel_token: CancellationToken,
    expected_headers: StdMutex<VecDeque<ExpectedHeadersResponse>>,
}

impl HeaderSyncPeerSession {
    pub(crate) fn new(session: &PeerStreamSession, direction: ServicePeerDirection) -> Self {
        Self::from_parts_with_direction(
            session.peer_id().clone(),
            direction,
            session.sender(),
            session.cancel_token(),
        )
    }

    #[cfg(test)]
    pub(crate) fn from_parts(
        peer_id: ZakuraPeerId,
        send: FramedSend,
        cancel_token: CancellationToken,
    ) -> Self {
        Self::from_parts_with_direction(peer_id, ServicePeerDirection::Inbound, send, cancel_token)
    }

    #[cfg(test)]
    pub(crate) fn from_parts_with_direction(
        peer_id: ZakuraPeerId,
        direction: ServicePeerDirection,
        send: FramedSend,
        cancel_token: CancellationToken,
    ) -> Self {
        Self {
            peer_id,
            direction,
            inner: Arc::new(HeaderSyncPeerSessionInner {
                send,
                cancel_token,
                expected_headers: StdMutex::new(VecDeque::new()),
            }),
        }
    }

    #[cfg(not(test))]
    fn from_parts_with_direction(
        peer_id: ZakuraPeerId,
        direction: ServicePeerDirection,
        send: FramedSend,
        cancel_token: CancellationToken,
    ) -> Self {
        Self {
            peer_id,
            direction,
            inner: Arc::new(HeaderSyncPeerSessionInner {
                send,
                cancel_token,
                expected_headers: StdMutex::new(VecDeque::new()),
            }),
        }
    }

    /// Authenticated peer identity for this header-sync session.
    pub fn peer_id(&self) -> &ZakuraPeerId {
        &self.peer_id
    }

    /// Direction of the underlying Zakura connection.
    pub fn direction(&self) -> ServicePeerDirection {
        self.direction
    }

    /// Peer disconnect/local shutdown cancellation token.
    pub fn cancel_token(&self) -> CancellationToken {
        self.inner.cancel_token.clone()
    }

    /// Send a typed status advertisement.
    pub fn try_send_status(&self, status: HeaderSyncStatus) -> Result<(), OrderedSendError> {
        self.try_send_message(HeaderSyncMessage::Status(status))
    }

    /// Send a typed header range request and record the expected response after queueing succeeds.
    pub fn try_send_get_headers(
        &self,
        start_height: block::Height,
        count: u32,
    ) -> Result<(), OrderedSendError> {
        let expected = ExpectedHeadersResponse::new(start_height, count)
            .map_err(|error| OrderedSendError::Encode(Box::new(error)))?;
        let mut expected_headers = self
            .inner
            .expected_headers
            .lock()
            .map_err(|_| OrderedSendError::Closed)?;
        self.try_send_message(HeaderSyncMessage::GetHeaders {
            start_height,
            count,
        })?;
        expected_headers.push_back(expected);
        Ok(())
    }

    /// Send a typed header range response.
    pub fn try_send_headers(
        &self,
        headers: Vec<Arc<block::Header>>,
    ) -> Result<(), OrderedSendError> {
        let body_sizes = vec![0; headers.len()];
        self.try_send_headers_with_sizes(headers, body_sizes)
    }

    /// Send a typed header range response with one advisory body-size hint per header.
    pub fn try_send_headers_with_sizes(
        &self,
        headers: Vec<Arc<block::Header>>,
        body_sizes: Vec<u32>,
    ) -> Result<(), OrderedSendError> {
        self.try_send_message(HeaderSyncMessage::Headers {
            headers,
            body_sizes,
        })
    }

    /// Send a typed full tip block announcement.
    pub fn try_send_new_block(&self, block: Arc<block::Block>) -> Result<(), OrderedSendError> {
        self.try_send_message(HeaderSyncMessage::NewBlock(block))
    }

    fn try_send_message(&self, msg: HeaderSyncMessage) -> Result<(), OrderedSendError> {
        let frame = msg
            .encode_frame()
            .map_err(|error| OrderedSendError::Encode(Box::new(error)))?;
        match self.inner.send.try_send(frame) {
            Ok(()) => Ok(()),
            Err(mpsc::error::TrySendError::Full(_frame)) => Err(OrderedSendError::Full),
            Err(mpsc::error::TrySendError::Closed(_frame)) => Err(OrderedSendError::Closed),
        }
    }

    fn pop_expected_headers_response(&self) -> Option<ExpectedHeadersResponse> {
        self.inner
            .expected_headers
            .lock()
            .expect("header-sync expected-response mutex is never poisoned")
            .pop_front()
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
            #[cfg(test)]
            HeaderSyncAction::SendMessage { .. } | HeaderSyncAction::ForwardNewBlock { .. } => {}
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
}

impl HeaderSyncService {
    pub(crate) fn new(header_sync: HeaderSyncHandle) -> Self {
        Self { header_sync }
    }
}

impl Service for HeaderSyncService {
    fn name(&self) -> &'static str {
        "header-sync"
    }

    fn streams(&self) -> &[Stream] {
        header_sync_streams()
    }

    fn wants_peer(
        &self,
        _peer: &ZakuraPeerId,
        _negotiated: u64,
        direction: ServicePeerDirection,
    ) -> bool {
        // Escalation is a local-room check. First-party summary usefulness is
        // advisory and is applied by header-sync candidate selection upstream.
        let snapshot = self.header_sync.peer_snapshot();
        match direction {
            ServicePeerDirection::Inbound => snapshot.inbound_slots_free > 0,
            ServicePeerDirection::Outbound => snapshot.outbound_slots_free > 0,
        }
    }

    fn add_peer(&self, mut peer: Peer) {
        let Some((recv, send)) = peer.take_stream(ZAKURA_STREAM_HEADER_SYNC) else {
            return;
        };

        let peer_id = peer.id.clone();
        let session = PeerStreamSession::new(
            peer_id.clone(),
            ZAKURA_STREAM_HEADER_SYNC,
            recv,
            send,
            peer.service_cancel_token(),
        );
        let service_cancel_token = session.cancel_token();
        let connection_cancel_token = peer.cancel_token();
        let header_sync_session = HeaderSyncPeerSession::new(&session, peer.direction);

        let _ = self
            .header_sync
            .send_lifecycle(HeaderSyncEvent::PeerConnected(header_sync_session.clone()));

        spawn_header_sync_sink(
            peer_id,
            session,
            header_sync_session,
            self.header_sync.clone(),
            service_cancel_token,
            connection_cancel_token,
        );
    }

    fn remove_peer(&self, peer: &ZakuraPeerId) {
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

        deliver_header_sync_frame(&self.header_sync, None, peer_id, frame)
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

    fn wants_peer(
        &self,
        peer: &ZakuraPeerId,
        negotiated: u64,
        direction: ServicePeerDirection,
    ) -> bool {
        self.inner.wants_peer(peer, negotiated, direction)
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
}

fn spawn_header_sync_sink(
    peer_id: ZakuraPeerId,
    session: PeerStreamSession,
    header_sync_session: HeaderSyncPeerSession,
    header_sync: HeaderSyncHandle,
    service_cancel_token: CancellationToken,
    connection_cancel_token: CancellationToken,
) {
    task::spawn(async move {
        let (_session_peer, _stream_kind, recv, _send, _session_cancel) = session.into_parts();
        let sink = Box::new(HeaderSyncSink {
            peer_id: peer_id.clone(),
            header_sync: header_sync.clone(),
            session: header_sync_session,
            cancel_token: service_cancel_token.clone(),
        });

        let result = sink.run(recv).await;

        match result {
            Ok(()) => {}
            Err(SinkReject::Protocol(error)) => {
                tracing::debug!(
                    ?error,
                    ?peer_id,
                    "header-sync stream rejected protocol-invalid frame"
                );
                connection_cancel_token.cancel();
            }
            Err(SinkReject::Local(error)) => {
                tracing::debug!(
                    ?error,
                    ?peer_id,
                    "header-sync stream could not deliver frame locally"
                );
            }
        }

        let _ = header_sync.send_lifecycle(HeaderSyncEvent::PeerDisconnected(peer_id));
    });
}

#[derive(Debug)]
struct HeaderSyncSink {
    peer_id: ZakuraPeerId,
    header_sync: HeaderSyncHandle,
    session: HeaderSyncPeerSession,
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

                match deliver_header_sync_frame(
                    &self.header_sync,
                    Some(&self.session),
                    self.peer_id.clone(),
                    frame,
                ) {
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
    session: Option<&HeaderSyncPeerSession>,
    peer_id: ZakuraPeerId,
    frame: Frame,
) -> Result<(), SinkReject> {
    if u8::try_from(frame.message_type).ok() == Some(MSG_HS_HEADERS) {
        let Some(expected) = session.and_then(HeaderSyncPeerSession::pop_expected_headers_response)
        else {
            let error = Arc::new(HeaderSyncWireError::UnsolicitedHeaders);
            let _ = header_sync.try_send(HeaderSyncEvent::WireProtocolFailure {
                peer: peer_id.clone(),
                reason: HeaderSyncMisbehavior::UnsolicitedHeaders,
                error: error.clone(),
            });
            let protocol_error =
                std::io::Error::new(std::io::ErrorKind::InvalidData, error.to_string());
            return Err(SinkReject::protocol(protocol_error));
        };

        let msg = match HeaderSyncMessage::decode_frame(
            frame,
            HeaderSyncDecodeContext::for_headers_response(expected, expected.count),
        ) {
            Ok(msg) => msg,
            Err(error) => {
                let protocol_error =
                    std::io::Error::new(std::io::ErrorKind::InvalidData, error.to_string());
                let _ = header_sync.try_send(HeaderSyncEvent::WireProtocolFailure {
                    peer: peer_id.clone(),
                    reason: HeaderSyncMisbehavior::MalformedMessage,
                    error: Arc::new(error),
                });
                return Err(SinkReject::protocol(protocol_error));
            }
        };

        return header_sync
            .try_send(HeaderSyncEvent::WireMessage { peer: peer_id, msg })
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
