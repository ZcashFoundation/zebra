//! Native discovery service (stream kind 4) on the Zakura transport.
//!
//! Discovery is a single long-lived ordered stream per peer. Each side runs a
//! [`DiscoverySink`] (the reader, which imports peer records and answers
//! `GetPeers`) and a [`DiscoverySource`] (the writer, which periodically gossips
//! the local self-record and asks for more peers). The wire format is the
//! [`DiscoveryMessage`] payload carried inside a generic transport [`Frame`]
//! (`message_type = DISCOVERY_FRAME_MESSAGE_TYPE`, `flags = 0`), identical to the
//! original native-discovery wire so peers interoperate.

use std::time::Duration;

use iroh::NodeId;
use tokio_util::sync::CancellationToken;

use crate::zakura::{
    BoxRunFuture, Frame, FramedRecv, FramedSend, Peer, Service, Sink, SinkReject, Source, Stream,
    StreamMode, ZakuraPeerId, LOCAL_MAX_CONTROL_FRAME_BYTES, ZAKURA_CAP_DISCOVERY,
};

use super::protocol::{
    DiscoveryBookError, DiscoveryMessage, DiscoveryRecordError, ZakuraDiscoveryHandle,
    ZakuraNodeRecord, MAX_DISCOVERY_RECORDS_PER_RESPONSE, ZAKURA_DISCOVERY_STREAM_VERSION,
    ZAKURA_STREAM_DISCOVERY,
};

/// Frame message type carrying a discovery payload (matches the native wire).
const DISCOVERY_FRAME_MESSAGE_TYPE: u16 = 1;

/// Minimum spacing between periodic discovery exchanges, regardless of config.
const MIN_DISCOVERY_REFRESH_INTERVAL: Duration = Duration::from_secs(1);

const DISCOVERY_SERVICE_STREAMS: [Stream; 1] = [Stream {
    kind: ZAKURA_STREAM_DISCOVERY,
    version: ZAKURA_DISCOVERY_STREAM_VERSION,
    // Advisory until the transport wires Stream::frame_cap end-to-end; the
    // authoritative inbound cap is app_frame_cap_for_stream_kind.
    frame_cap: LOCAL_MAX_CONTROL_FRAME_BYTES,
    capability: ZAKURA_CAP_DISCOVERY,
    mode: StreamMode::Ordered,
}];

/// Service-declared streams for native discovery.
pub(crate) fn discovery_streams() -> &'static [Stream] {
    &DISCOVERY_SERVICE_STREAMS
}

/// Native discovery service backed by a [`ZakuraDiscoveryHandle`] runtime.
#[derive(Clone, Debug)]
pub struct DiscoveryService {
    handle: ZakuraDiscoveryHandle,
}

impl DiscoveryService {
    /// Builds a discovery service driven by `handle`.
    pub fn new(handle: ZakuraDiscoveryHandle) -> Self {
        Self { handle }
    }

    /// Returns the underlying discovery runtime handle.
    pub fn handle(&self) -> &ZakuraDiscoveryHandle {
        &self.handle
    }
}

impl Service for DiscoveryService {
    fn name(&self) -> &'static str {
        "discovery"
    }

    fn streams(&self) -> &[Stream] {
        discovery_streams()
    }

    fn add_peer(&self, mut peer: Peer) {
        let Some((recv, send)) = peer.take_stream(ZAKURA_STREAM_DISCOVERY) else {
            return;
        };
        let Some(peer_node_id) = node_id_from_peer_id(&peer.id) else {
            // A peer id that is not a 32-byte node id cannot be a discovery
            // author; drop the stream without registering an exchange.
            return;
        };
        let cancel = peer.cancel_token();

        let sink = DiscoverySink {
            handle: self.handle.clone(),
            peer_node_id,
            send: send.clone(),
        };
        let sink_cancel = cancel.clone();
        tokio::spawn(async move {
            match Box::new(sink).run(recv).await {
                Ok(()) => {}
                Err(SinkReject::Protocol(error)) => {
                    tracing::debug!(
                        ?error,
                        "Zakura discovery stream rejected protocol-invalid frame"
                    );
                    sink_cancel.cancel();
                }
                Err(SinkReject::Local(error)) => {
                    tracing::debug!(?error, "Zakura discovery stream stopped on local error");
                }
            }
        });

        let source = DiscoverySource {
            handle: self.handle.clone(),
            cancel,
        };
        tokio::spawn(async move {
            Box::new(source).run(send).await;
        });
    }

    fn remove_peer(&self, _peer: &ZakuraPeerId) {
        // The runtime tracks the connected set through the supervisor watch it
        // was constructed with; active-service queries cross-reference it, so a
        // disconnect needs no explicit bookkeeping here.
    }
}

/// Reader half of the discovery stream: imports peer records and answers queries.
struct DiscoverySink {
    handle: ZakuraDiscoveryHandle,
    peer_node_id: NodeId,
    send: FramedSend,
}

impl Sink for DiscoverySink {
    fn run(self: Box<Self>, mut recv: FramedRecv) -> BoxRunFuture<'static, Result<(), SinkReject>> {
        Box::pin(async move {
            while let Some(frame) = recv.recv().await {
                self.handle_frame(frame).await?;
            }
            Ok(())
        })
    }
}

impl DiscoverySink {
    async fn handle_frame(&self, frame: Frame) -> Result<(), SinkReject> {
        let message = decode_discovery_frame(&frame).map_err(SinkReject::protocol)?;
        match message {
            DiscoveryMessage::Hello { record } => self.handle_hello(record).await,
            DiscoveryMessage::GetPeers {
                limit,
                wanted_services,
                exclude_node_ids,
            } => {
                let records = self
                    .handle
                    .sample_peers(usize::from(limit), &wanted_services, &exclude_node_ids)
                    .await;
                self.send_message(DiscoveryMessage::Peers { records }).await
            }
            DiscoveryMessage::Peers { records } => {
                self.handle
                    .import_peer_records(records, Some(self.peer_node_id))
                    .await;
                Ok(())
            }
            DiscoveryMessage::GetServices { .. } | DiscoveryMessage::Services { .. } => {
                // Service discovery rides the self-record service list, not a
                // dedicated message exchange; an explicit service message is a
                // protocol violation.
                Err(SinkReject::protocol(
                    "Zakura discovery service messages are not supported",
                ))
            }
        }
    }

    async fn handle_hello(&self, record: ZakuraNodeRecord) -> Result<(), SinkReject> {
        if record.body.node_id != self.peer_node_id {
            return Err(SinkReject::protocol(
                "Zakura discovery hello authored by a different node id",
            ));
        }
        match self
            .handle
            .import_connected_peer_record(record, self.peer_node_id)
            .await
        {
            Ok(_) => Ok(()),
            Err(error) if is_advisory_self_record_import_error(&error) => {
                tracing::debug!(?error, "ignoring advisory discovery hello import error");
                Ok(())
            }
            Err(error) => Err(SinkReject::protocol(error)),
        }
    }

    async fn send_message(&self, message: DiscoveryMessage) -> Result<(), SinkReject> {
        send_discovery_message(&self.send, message).await
    }
}

/// Writer half of the discovery stream: periodic self-record gossip + peer asks.
struct DiscoverySource {
    handle: ZakuraDiscoveryHandle,
    cancel: CancellationToken,
}

impl Source for DiscoverySource {
    fn run(self: Box<Self>, send: FramedSend) -> BoxRunFuture<'static, ()> {
        Box::pin(async move {
            if self.exchange(&send).await.is_err() {
                return;
            }
            let refresh = self
                .handle
                .refresh_interval()
                .await
                .max(MIN_DISCOVERY_REFRESH_INTERVAL);
            loop {
                tokio::select! {
                    biased;
                    _ = self.cancel.cancelled() => return,
                    _ = tokio::time::sleep(refresh) => {}
                }
                if self.exchange(&send).await.is_err() {
                    return;
                }
            }
        })
    }
}

impl DiscoverySource {
    /// Gossips the current self-record and asks the peer for more peers.
    ///
    /// Returns `Err(())` once the stream's send side is gone, so the caller
    /// stops the periodic loop.
    async fn exchange(&self, send: &FramedSend) -> Result<(), ()> {
        let hello = DiscoveryMessage::Hello {
            record: (*self.handle.current_self_record()).clone(),
        };
        send_discovery_message(send, hello).await.map_err(|_| ())?;

        let limit = self
            .handle
            .peer_sample_limit()
            .await
            .min(MAX_DISCOVERY_RECORDS_PER_RESPONSE);
        // `peer_sample_limit` is bounded by MAX_DISCOVERY_RECORDS_PER_RESPONSE
        // (<= u16::MAX), so the cast cannot truncate.
        let get_peers = DiscoveryMessage::GetPeers {
            limit: limit as u16,
            wanted_services: Vec::new(),
            exclude_node_ids: self.handle.peer_sample_exclusions().await,
        };
        send_discovery_message(send, get_peers)
            .await
            .map_err(|_| ())
    }
}

/// Encodes and sends a discovery message as a transport frame.
async fn send_discovery_message(
    send: &FramedSend,
    message: DiscoveryMessage,
) -> Result<(), SinkReject> {
    let payload = message.encode().map_err(SinkReject::local)?;
    send.send(Frame {
        message_type: DISCOVERY_FRAME_MESSAGE_TYPE,
        flags: 0,
        payload,
    })
    .await
    .map_err(|_| SinkReject::local("Zakura discovery send channel closed"))
}

/// Decodes a discovery message from a transport frame, rejecting a frame whose
/// envelope is not a discovery payload.
fn decode_discovery_frame(frame: &Frame) -> Result<DiscoveryMessage, crate::BoxError> {
    if frame.message_type != DISCOVERY_FRAME_MESSAGE_TYPE || frame.flags != 0 {
        return Err(format!(
            "unexpected discovery frame envelope (message_type={}, flags={})",
            frame.message_type, frame.flags
        )
        .into());
    }
    DiscoveryMessage::decode(&frame.payload).map_err(Into::into)
}

/// Returns the iroh node id encoded by a discovery peer id, if it is a 32-byte
/// node id.
fn node_id_from_peer_id(peer_id: &ZakuraPeerId) -> Option<NodeId> {
    let bytes: [u8; 32] = peer_id.as_bytes().try_into().ok()?;
    NodeId::from_bytes(&bytes).ok()
}

/// A peer-hello import error that should be logged and ignored rather than
/// closing the live connection. These mean the peer's record is not locally
/// dialable or has drifted out of the freshness window, neither of which is the
/// connected peer's fault.
fn is_advisory_self_record_import_error(error: &DiscoveryBookError) -> bool {
    matches!(
        error,
        DiscoveryBookError::NoUsableDirectAddress
            | DiscoveryBookError::NonDialableDirectAddress { .. }
            | DiscoveryBookError::Record(DiscoveryRecordError::Expired)
            | DiscoveryBookError::Record(DiscoveryRecordError::FarFutureExpiry)
    )
}
