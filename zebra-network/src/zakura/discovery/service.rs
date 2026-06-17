//! Native discovery service (stream kind 4) on the Zakura transport.
//!
//! Discovery is a single long-lived ordered stream per peer. Each side runs a
//! [`DiscoverySink`] (the reader, which imports peer records and answers
//! `GetPeers`) and a [`DiscoverySource`] (the writer, which periodically gossips
//! the local self-record and asks for more peers). The wire format is the
//! [`DiscoveryMessage`] payload carried inside a generic transport [`Frame`]
//! (`message_type = DISCOVERY_FRAME_MESSAGE_TYPE`, `flags = 0`), identical to the
//! original native-discovery wire so peers interoperate.

use std::{
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::Duration,
};

use iroh::NodeId;
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

use crate::zakura::{
    handle_pipe_exit, spawn_supervised_peer_task, spawn_supervised_pipe, BlockSyncHandle, Flow,
    Frame, FramedRecv, FramedSend, HeaderSyncEvent, HeaderSyncHandle, OrderedSendError, Peer,
    PeerStreamSession, Pipe, Service, ServiceAdmissionDecision, ServicePeerDirection, SinkReject,
    Stream, StreamMode, ZakuraPeerId, LOCAL_MAX_CONTROL_FRAME_BYTES, ZAKURA_CAP_DISCOVERY,
    ZAKURA_CAP_HEADER_SYNC,
};

#[cfg(test)]
use super::pipe::decode_discovery_frame;
use super::pipe::{discovery_pipe, DsEnv, DsLocal, DISCOVERY_FRAME_MESSAGE_TYPE};
use super::protocol::{
    BlockSyncServiceSummary, DiscoveryBookError, DiscoveryMessage, DiscoveryRecordError,
    GetServices, HeaderSyncServiceSummary, ServiceSummaryEnvelope, Services, ZakuraDiscoveryHandle,
    ZakuraNodeRecord, ZakuraServiceId, MAX_DISCOVERY_RECORDS_PER_RESPONSE,
    ZAKURA_DISCOVERY_STREAM_VERSION, ZAKURA_STREAM_DISCOVERY,
};

/// Maximum time discovery waits for first-party exchange responses before releasing the session.
const DISCOVERY_EXCHANGE_SETTLE_TIMEOUT: Duration = Duration::from_secs(2);

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

/// Cloneable typed sender for one native discovery ordered stream.
#[derive(Clone, Debug)]
pub struct DiscoveryPeerSession {
    peer_id: ZakuraPeerId,
    direction: ServicePeerDirection,
    send: FramedSend,
    cancel: CancellationToken,
}

impl DiscoveryPeerSession {
    fn new(session: &PeerStreamSession, direction: ServicePeerDirection) -> Self {
        Self {
            peer_id: session.peer_id().clone(),
            direction,
            send: session.sender(),
            cancel: session.cancel_token(),
        }
    }

    /// Authenticated peer identity for this discovery stream.
    pub fn peer_id(&self) -> &ZakuraPeerId {
        &self.peer_id
    }

    /// Direction of the underlying Zakura connection.
    pub fn direction(&self) -> ServicePeerDirection {
        self.direction
    }

    /// Peer disconnect/local shutdown cancellation token.
    pub fn cancel_token(&self) -> CancellationToken {
        self.cancel.clone()
    }

    /// Send this node's signed self-record.
    pub fn try_send_hello(&self, record: ZakuraNodeRecord) -> Result<(), OrderedSendError> {
        self.try_send_message(DiscoveryMessage::Hello { record })
    }

    /// Ask this peer for more peer records.
    pub fn try_send_get_peers(
        &self,
        limit: u16,
        wanted_services: Vec<ZakuraServiceId>,
        exclude_node_ids: Vec<NodeId>,
    ) -> Result<(), OrderedSendError> {
        self.try_send_message(DiscoveryMessage::GetPeers {
            limit,
            wanted_services,
            exclude_node_ids,
        })
    }

    /// Send peer records to this peer.
    pub fn try_send_peers(&self, records: Vec<ZakuraNodeRecord>) -> Result<(), OrderedSendError> {
        self.try_send_message(DiscoveryMessage::Peers { records })
    }

    /// Ask this peer for its own live service summaries.
    pub fn try_send_get_services(
        &self,
        wanted_services: Vec<ZakuraServiceId>,
    ) -> Result<(), OrderedSendError> {
        self.try_send_message(DiscoveryMessage::GetServices(GetServices {
            wanted_services,
        }))
    }

    /// Send this node's first-party live service summaries.
    pub fn try_send_services(&self, services: Services) -> Result<(), OrderedSendError> {
        self.try_send_message(DiscoveryMessage::Services(services))
    }

    fn try_send_message(&self, message: DiscoveryMessage) -> Result<(), OrderedSendError> {
        let payload = message
            .encode()
            .map_err(|error| OrderedSendError::Encode(Box::new(error)))?;
        match self.send.try_send(Frame {
            message_type: DISCOVERY_FRAME_MESSAGE_TYPE,
            flags: 0,
            payload,
        }) {
            Ok(()) => Ok(()),
            Err(tokio::sync::mpsc::error::TrySendError::Full(_frame)) => {
                Err(OrderedSendError::Full)
            }
            Err(tokio::sync::mpsc::error::TrySendError::Closed(_frame)) => {
                Err(OrderedSendError::Closed)
            }
        }
    }
}

/// Native discovery service backed by a [`ZakuraDiscoveryHandle`] runtime.
#[derive(Clone, Debug)]
pub struct DiscoveryService {
    handle: ZakuraDiscoveryHandle,
    header_sync: Option<HeaderSyncHandle>,
    block_sync: Option<BlockSyncHandle>,
}

impl DiscoveryService {
    /// Builds a discovery service driven by `handle`.
    pub fn new(handle: ZakuraDiscoveryHandle) -> Self {
        Self {
            handle,
            header_sync: None,
            block_sync: None,
        }
    }

    /// Builds a discovery service with header-sync and block-sync summary providers.
    pub(crate) fn with_sync_services(
        handle: ZakuraDiscoveryHandle,
        header_sync: HeaderSyncHandle,
        block_sync: Option<BlockSyncHandle>,
    ) -> Self {
        Self {
            handle,
            header_sync: Some(header_sync),
            block_sync,
        }
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

    fn wants_peer(
        &self,
        _peer: &ZakuraPeerId,
        _negotiated: u64,
        direction: ServicePeerDirection,
    ) -> bool {
        // Discovery escalation only checks this reactor's local room; live
        // summaries are first-party advisory data imported by the runtime.
        let snapshot = self.handle.peer_snapshot();
        match direction {
            ServicePeerDirection::Inbound => snapshot.inbound_slots_free > 0,
            ServicePeerDirection::Outbound => snapshot.outbound_slots_free > 0,
        }
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
        let session = PeerStreamSession::new(
            peer.id.clone(),
            ZAKURA_STREAM_DISCOVERY,
            recv,
            send,
            peer.service_cancel_token(),
        );
        let discovery_session = DiscoveryPeerSession::new(&session, peer.direction);
        let service_cancel = discovery_session.cancel_token();
        let connection_cancel = peer.cancel_token();
        let other_service_negotiated =
            peer.negotiated & !(ZAKURA_CAP_DISCOVERY | ZAKURA_CAP_HEADER_SYNC) != 0;
        let (_peer_id, _stream_kind, recv, _send, _session_cancel) = session.into_parts();

        let handle = self.handle.clone();
        let header_sync = self.header_sync.clone();
        let block_sync = self.block_sync.clone();
        // SR-1: a panic in the admission task (before it hands off to the
        // exchange) must still disconnect this one peer and cancel its discovery
        // session instead of leaving admitted state behind a half-live
        // connection. Normal/parked exits cancel `service_cancel` inline below;
        // `on_panic` covers the unwind path only.
        let admit_peer_id = discovery_session.peer_id().clone();
        let panic_service_cancel = service_cancel.clone();
        let panic_connection_cancel = connection_cancel.clone();
        spawn_supervised_peer_task(
            admit_peer_id,
            || {},
            move || {
                panic_service_cancel.cancel();
                panic_connection_cancel.cancel();
            },
            async move {
                let decision = handle
                    .admit_peer(
                        discovery_session.peer_id().clone(),
                        discovery_session.direction(),
                    )
                    .await;
                if decision != ServiceAdmissionDecision::Admit {
                    tracing::debug!(
                        peer = ?discovery_session.peer_id(),
                        direction = ?discovery_session.direction(),
                        ?decision,
                        "locally parking Zakura discovery service session"
                    );
                    service_cancel.cancel();
                    return;
                }

                spawn_discovery_exchange(DiscoveryExchangeStart {
                    handle,
                    header_sync,
                    block_sync,
                    peer_node_id,
                    discovery_session,
                    recv,
                    service_cancel,
                    connection_cancel,
                    other_service_negotiated,
                });
            },
        );
    }

    fn remove_peer(&self, peer: &ZakuraPeerId) {
        let handle = self.handle.clone();
        let peer = peer.clone();
        tokio::spawn(async move {
            handle.remove_peer(&peer).await;
        });
    }
}

struct DiscoveryExchangeStart {
    handle: ZakuraDiscoveryHandle,
    header_sync: Option<HeaderSyncHandle>,
    block_sync: Option<BlockSyncHandle>,
    peer_node_id: NodeId,
    discovery_session: DiscoveryPeerSession,
    recv: FramedRecv,
    service_cancel: CancellationToken,
    connection_cancel: CancellationToken,
    other_service_negotiated: bool,
}

fn spawn_discovery_exchange(start: DiscoveryExchangeStart) {
    let DiscoveryExchangeStart {
        handle,
        header_sync,
        block_sync,
        peer_node_id,
        discovery_session,
        recv,
        service_cancel,
        connection_cancel,
        other_service_negotiated,
    } = start;
    let peer_id = discovery_session.peer_id().clone();
    let progress = Arc::new(DiscoveryExchangeProgress::default());
    let source_header_sync = header_sync.clone();
    let sink = DiscoverySink {
        handle: handle.clone(),
        header_sync,
        block_sync,
        peer_node_id,
        session: discovery_session.clone(),
        progress: progress.clone(),
    };
    let sink_service_cancel = service_cancel.clone();
    let reject_connection_cancel = connection_cancel.clone();
    let panic_connection_cancel = connection_cancel.clone();
    let sink_peer_id = peer_id.clone();
    // A protocol reject is fatal to the connection; normal/parked exits leave it
    // for the source task to tear down once it knows no other service owns the
    // peer (below). Panic teardown is in `on_panic`.
    let pipe = async move {
        let mut pipe = discovery_pipe(sink_peer_id);
        handle_pipe_exit(
            "discovery",
            &reject_connection_cancel,
            run_discovery_pipe(&mut pipe, recv, sink).await,
        );
    };
    let on_panic = move || panic_connection_cancel.cancel();
    // Let the returned handle drop to detach the supervised reader task; the
    // `PipeTeardown` still runs on every exit path.
    spawn_supervised_pipe(peer_id.clone(), sink_service_cancel, || {}, on_panic, pipe);

    let source = DiscoverySource {
        handle: handle.clone(),
        session: discovery_session,
        progress,
    };
    // SR-1: a panic in the source task skips its `service_cancel.cancel()`,
    // `handle.remove_peer()`, and discovery-only connection cancellation,
    // leaving admitted discovery state behind a half-live connection. On the
    // unwind path, disconnect this one peer; the connection teardown then drives
    // the async `remove_peer` through the registry. Normal exits run the inline
    // cleanup below, so `on_panic` is the panic-only path.
    let source_task_peer_id = peer_id.clone();
    let panic_source_service_cancel = service_cancel.clone();
    let panic_source_connection_cancel = connection_cancel.clone();
    spawn_supervised_peer_task(
        source_task_peer_id,
        || {},
        move || {
            panic_source_service_cancel.cancel();
            panic_source_connection_cancel.cancel();
        },
        async move {
            let exchanged = source.run().await;
            if exchanged {
                handle.mark_short_lived_exchange(&peer_node_id).await;
            }
            service_cancel.cancel();
            handle.remove_peer(&peer_id).await;
            if exchanged
                && !peer_has_other_service_owner(
                    source_header_sync.as_ref(),
                    peer_node_id,
                    other_service_negotiated,
                )
            {
                connection_cancel.cancel();
            }
        },
    );
}

/// Reader half of the discovery stream: imports peer records and answers queries.
struct DiscoverySink {
    handle: ZakuraDiscoveryHandle,
    header_sync: Option<HeaderSyncHandle>,
    block_sync: Option<BlockSyncHandle>,
    peer_node_id: NodeId,
    session: DiscoveryPeerSession,
    progress: Arc<DiscoveryExchangeProgress>,
}

async fn run_discovery_pipe(
    pipe: &mut Pipe<DsLocal, DsEnv>,
    mut recv: FramedRecv,
    sink: DiscoverySink,
) -> Result<(), SinkReject> {
    let cancel = sink.session.cancel_token();
    loop {
        let frame = tokio::select! {
            biased;
            _ = cancel.cancelled() => return Ok(()),
            frame = recv.recv() => frame,
        };
        let Some(frame) = frame else {
            return Ok(());
        };

        match pipe.run_one(frame) {
            Flow::Continue(()) | Flow::Done => {}
            Flow::Reject(reject) => return Err(reject),
        }

        let Some(message) = pipe.local_mut().take_decoded() else {
            continue;
        };
        sink.handle_message(message).await?;
    }
}

impl DiscoverySink {
    async fn handle_message(&self, message: DiscoveryMessage) -> Result<(), SinkReject> {
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
                self.send_peers(records)
            }
            DiscoveryMessage::Peers { records } => {
                self.handle
                    .import_peer_records(records, Some(self.peer_node_id))
                    .await;
                self.progress.mark_peers();
                Ok(())
            }
            DiscoveryMessage::GetServices(query) => {
                let services = self.local_services_response(query).await?;
                self.send_services(services)
            }
            DiscoveryMessage::Services(services) => self.handle_services(services).await,
        }
    }

    async fn local_services_response(&self, query: GetServices) -> Result<Services, SinkReject> {
        let mut summaries = Vec::new();

        if service_wanted(&query.wanted_services, &ZakuraServiceId::header_sync()) {
            if let Some(header_sync) = &self.header_sync {
                let (best_height, best_hash) = header_sync.best_header_tip();
                let summary = HeaderSyncServiceSummary::from_snapshot(
                    best_height,
                    best_hash,
                    None,
                    true,
                    header_sync.peer_snapshot(),
                );
                summaries.push(
                    ServiceSummaryEnvelope::header_sync(&summary).map_err(SinkReject::local)?,
                );
            }
        }

        if service_wanted(&query.wanted_services, &ZakuraServiceId::discovery()) {
            let summary = self.handle.local_discovery_summary().await;
            summaries.push(ServiceSummaryEnvelope::discovery(&summary).map_err(SinkReject::local)?);
        }

        if service_wanted(&query.wanted_services, &ZakuraServiceId::block_sync()) {
            if let Some(block_sync) = &self.block_sync {
                let summary = BlockSyncServiceSummary::from_status_and_snapshot(
                    block_sync.local_status(),
                    block_sync.peer_snapshot(),
                );
                summaries
                    .push(ServiceSummaryEnvelope::block_sync(&summary).map_err(SinkReject::local)?);
            }
        }

        Ok(self.handle.local_services_response(summaries))
    }

    async fn handle_services(&self, services: Services) -> Result<(), SinkReject> {
        if services.node_id != self.peer_node_id {
            return Err(SinkReject::protocol(
                "Zakura discovery SERVICES authored by a different node id",
            ));
        }

        let header_summaries =
            decode_header_sync_summaries(&services).map_err(SinkReject::protocol)?;
        self.handle
            .import_connected_peer_services(services, self.peer_node_id)
            .await
            .map_err(SinkReject::protocol)?;
        self.progress.mark_services();

        if let Some(header_sync) = &self.header_sync {
            for summary in header_summaries {
                if let Err(error) = header_sync
                    .send(HeaderSyncEvent::AdvisoryHeaderSummary {
                        peer: self.session.peer_id().clone(),
                        summary,
                    })
                    .await
                {
                    tracing::debug!(
                        ?error,
                        peer = ?self.session.peer_id(),
                        "failed to queue first-party Zakura header-sync advisory summary"
                    );
                    break;
                }
            }
        }

        Ok(())
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
        }?;
        self.progress.mark_hello();
        Ok(())
    }

    fn send_peers(&self, records: Vec<ZakuraNodeRecord>) -> Result<(), SinkReject> {
        match self.session.try_send_peers(records) {
            Ok(()) | Err(OrderedSendError::Full) => Ok(()),
            Err(OrderedSendError::Closed) => {
                Err(SinkReject::local("Zakura discovery send channel closed"))
            }
            Err(OrderedSendError::Encode(error)) => Err(SinkReject::local(error)),
        }
    }

    fn send_services(&self, services: Services) -> Result<(), SinkReject> {
        match self.session.try_send_services(services) {
            Ok(()) | Err(OrderedSendError::Full) => Ok(()),
            Err(OrderedSendError::Closed) => {
                Err(SinkReject::local("Zakura discovery send channel closed"))
            }
            Err(OrderedSendError::Encode(error)) => Err(SinkReject::local(error)),
        }
    }
}

fn service_wanted(wanted_services: &[ZakuraServiceId], service_id: &ZakuraServiceId) -> bool {
    wanted_services.is_empty() || wanted_services.iter().any(|wanted| wanted == service_id)
}

fn decode_header_sync_summaries(
    services: &Services,
) -> Result<Vec<HeaderSyncServiceSummary>, crate::BoxError> {
    let mut summaries = Vec::new();
    for envelope in &services.summaries {
        if let Some(summary) = envelope.decode_header_sync()? {
            summaries.push(summary);
        }
    }
    Ok(summaries)
}

/// Writer half of the discovery stream: periodic self-record gossip + peer asks.
struct DiscoverySource {
    handle: ZakuraDiscoveryHandle,
    session: DiscoveryPeerSession,
    progress: Arc<DiscoveryExchangeProgress>,
}

impl DiscoverySource {
    async fn run(self) -> bool {
        if self.exchange().await.is_err() {
            return false;
        }
        let cancel = self.session.cancel_token();
        tokio::select! {
            biased;
            _ = cancel.cancelled() => {}
            _ = self.progress.wait_complete() => {}
            _ = tokio::time::sleep(DISCOVERY_EXCHANGE_SETTLE_TIMEOUT) => {}
        }
        true
    }

    /// Gossips the current self-record and asks the peer for records and services.
    ///
    /// Returns `Err(())` once the stream's send side is gone, so the caller
    /// stops the periodic loop.
    async fn exchange(&self) -> Result<(), ()> {
        let record = (*self.handle.current_self_record()).clone();
        self.handle_send_result(self.session.try_send_hello(record))?;

        let limit = self
            .handle
            .peer_sample_limit()
            .await
            .min(MAX_DISCOVERY_RECORDS_PER_RESPONSE);
        // `peer_sample_limit` is bounded by MAX_DISCOVERY_RECORDS_PER_RESPONSE
        // (<= u16::MAX), so the cast cannot truncate.
        let exclude_node_ids = self.handle.peer_sample_exclusions().await;
        self.handle_send_result(self.session.try_send_get_peers(
            limit as u16,
            Vec::new(),
            exclude_node_ids,
        ))?;

        self.handle_send_result(self.session.try_send_get_services(Vec::new()))
    }

    fn handle_send_result(&self, result: Result<(), OrderedSendError>) -> Result<(), ()> {
        match result {
            Ok(()) | Err(OrderedSendError::Full) => Ok(()),
            Err(OrderedSendError::Closed) => Err(()),
            Err(OrderedSendError::Encode(error)) => {
                tracing::debug!(
                    ?error,
                    peer = ?self.session.peer_id(),
                    "failed to encode Zakura discovery message"
                );
                Ok(())
            }
        }
    }
}

#[derive(Default)]
struct DiscoveryExchangeProgress {
    hello: AtomicBool,
    peers: AtomicBool,
    services: AtomicBool,
    notify: Notify,
}

impl DiscoveryExchangeProgress {
    fn mark_hello(&self) {
        self.hello.store(true, Ordering::Relaxed);
        self.notify.notify_waiters();
    }

    fn mark_peers(&self) {
        self.peers.store(true, Ordering::Relaxed);
        self.notify.notify_waiters();
    }

    fn mark_services(&self) {
        self.services.store(true, Ordering::Relaxed);
        self.notify.notify_waiters();
    }

    fn complete(&self) -> bool {
        self.hello.load(Ordering::Relaxed)
            && self.peers.load(Ordering::Relaxed)
            && self.services.load(Ordering::Relaxed)
    }

    async fn wait_complete(&self) {
        while !self.complete() {
            self.notify.notified().await;
        }
    }
}

fn peer_has_other_service_owner(
    header_sync: Option<&HeaderSyncHandle>,
    peer_node_id: NodeId,
    other_service_negotiated: bool,
) -> bool {
    if other_service_negotiated {
        return true;
    }

    header_sync.is_some_and(|header_sync| {
        header_sync
            .candidate_state()
            .admitted_node_ids
            .contains(&peer_node_id)
    })
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

#[cfg(test)]
mod tests {
    use std::{
        collections::HashMap,
        net::{IpAddr, Ipv4Addr, SocketAddr},
        time::{Duration, SystemTime, UNIX_EPOCH},
    };

    use iroh::SecretKey;
    use tokio::{sync::watch, task::JoinHandle};

    use super::*;
    use crate::zakura::discovery::protocol::{
        DiscoveryServiceSummary, ZakuraLiveServiceSummary, ZakuraNodeRecordBody,
    };
    use crate::zakura::{
        framed_channel, spawn_block_sync_reactor, spawn_header_sync_reactor, BlockSyncFrontiers,
        BlockSyncStartup, HeaderSyncAction, HeaderSyncFrontiers, HeaderSyncMessage,
        HeaderSyncPeerSession, HeaderSyncStartup, HeaderSyncStatus, ServicePeerLimits,
        ZakuraBlockSyncConfig, ZakuraDiscoveryConfig, ZakuraDiscoveryLocalConfig,
        ZakuraHandshakeConfig, ZakuraHeaderSyncConfig, LOCAL_MAX_MESSAGE_BYTES,
        MAX_BS_RESPONSE_BYTES, ZAKURA_CAP_BLOCK_SYNC, ZAKURA_CAP_DISCOVERY, ZAKURA_CAP_HEADER_SYNC,
    };
    use zebra_chain::{block, parameters::Network};

    struct HeaderAdvisoryFixture {
        discovery_handle: ZakuraDiscoveryHandle,
        header_sync: HeaderSyncHandle,
        header_actions: tokio::sync::mpsc::Receiver<HeaderSyncAction>,
        header_task: JoinHandle<()>,
        peer_node_id: NodeId,
        peer_id: ZakuraPeerId,
        peer_send: FramedSend,
        _peer_recv: FramedRecv,
    }

    impl Drop for HeaderAdvisoryFixture {
        fn drop(&mut self) {
            self.header_task.abort();
        }
    }

    fn current_test_unix_secs() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock is after Unix epoch")
            .as_secs()
    }

    fn header_summary(best_height: block::Height) -> HeaderSyncServiceSummary {
        HeaderSyncServiceSummary {
            best_height,
            best_hash: block::Hash([7; 32]),
            finalized_height: None,
            serving_headers: true,
            inbound_slots_free: 1,
            inbound_slots_max: 1,
            outbound_slots_free: 1,
            outbound_slots_max: 1,
        }
    }

    fn spawn_test_header_sync() -> Result<
        (
            HeaderSyncHandle,
            tokio::sync::mpsc::Receiver<HeaderSyncAction>,
            JoinHandle<()>,
        ),
        crate::BoxError,
    > {
        let network = Network::new_regtest(Default::default());
        let anchor = (block::Height(0), network.genesis_hash());
        let mut startup = HeaderSyncStartup::new(
            network,
            anchor,
            HeaderSyncFrontiers {
                finalized_height: anchor.0,
                verified_block_tip: anchor.0,
                verified_block_hash: anchor.1,
            },
            Some(anchor),
            ZakuraHeaderSyncConfig::default(),
            LOCAL_MAX_MESSAGE_BYTES,
        );
        startup.range_state_actions_enabled = true;
        spawn_header_sync_reactor(startup).map_err(Into::into)
    }

    fn signed_header_sync_record(
        secret_key: &SecretKey,
        handshake: &ZakuraHandshakeConfig,
    ) -> Result<ZakuraNodeRecord, crate::BoxError> {
        let body = ZakuraNodeRecordBody {
            node_id: secret_key.public(),
            direct_addrs: vec![SocketAddr::new(
                IpAddr::V4(Ipv4Addr::new(192, 0, 2, 44)),
                8233,
            )],
            services: vec![ZakuraServiceId::header_sync()],
            zakura_protocol_min: handshake.zakura_protocol_min,
            zakura_protocol_max: handshake.zakura_protocol_max,
            network_id: handshake.network_id,
            chain_id: handshake.chain_id,
            sequence: 1,
            expires_at_unix_secs: current_test_unix_secs().saturating_add(60),
        };
        Ok(ZakuraNodeRecord::sign(body, secret_key)?)
    }

    fn spawn_header_advisory_fixture(
        peer_seed: u8,
    ) -> Result<HeaderAdvisoryFixture, crate::BoxError> {
        let (connected_tx, connected_rx) = watch::channel(Vec::new());
        let handshake = ZakuraHandshakeConfig::for_network(&Network::Mainnet);
        let local_secret = SecretKey::from_bytes(&[31u8; 32]);
        let discovery_handle = ZakuraDiscoveryHandle::new(
            ZakuraDiscoveryLocalConfig {
                secret_key: local_secret,
                direct_addrs: Vec::new(),
                services: vec![ZakuraServiceId::discovery()],
                zakura_protocol_min: handshake.zakura_protocol_min,
                zakura_protocol_max: handshake.zakura_protocol_max,
                network_id: handshake.network_id,
                chain_id: handshake.chain_id,
                last_authored_sequence: None,
            },
            ZakuraDiscoveryConfig::default(),
            connected_rx,
        )?;
        let (header_sync, header_actions, header_task) = spawn_test_header_sync()?;
        let service = DiscoveryService::with_sync_services(
            discovery_handle.clone(),
            header_sync.clone(),
            None,
        );
        let peer_node_id = SecretKey::from_bytes(&[peer_seed; 32]).public();
        let peer_id = ZakuraPeerId::new(peer_node_id.as_bytes().to_vec())?;
        connected_tx.send_replace(vec![peer_id.clone()]);

        let (peer_send, service_recv) = framed_channel(8);
        let (service_send, peer_recv) = framed_channel(8);
        let streams = HashMap::from([(ZAKURA_STREAM_DISCOVERY, (service_recv, service_send))]);

        service.add_peer(Peer::new(
            peer_id.clone(),
            None,
            ZAKURA_CAP_DISCOVERY,
            streams,
            CancellationToken::new(),
        ));

        Ok(HeaderAdvisoryFixture {
            discovery_handle,
            header_sync,
            header_actions,
            header_task,
            peer_node_id,
            peer_id,
            peer_send,
            _peer_recv: peer_recv,
        })
    }

    async fn send_discovery_message(
        fixture: &HeaderAdvisoryFixture,
        message: DiscoveryMessage,
    ) -> Result<(), crate::BoxError> {
        fixture
            .peer_send
            .send(Frame {
                message_type: DISCOVERY_FRAME_MESSAGE_TYPE,
                flags: 0,
                payload: message.encode()?,
            })
            .await?;
        Ok(())
    }

    fn discovery_frame(message: DiscoveryMessage) -> Result<Frame, crate::BoxError> {
        Ok(Frame {
            message_type: DISCOVERY_FRAME_MESSAGE_TYPE,
            flags: 0,
            payload: message.encode()?,
        })
    }

    fn signed_discovery_record(
        secret_key: &SecretKey,
        handshake: &ZakuraHandshakeConfig,
    ) -> Result<ZakuraNodeRecord, crate::BoxError> {
        let body = ZakuraNodeRecordBody {
            node_id: secret_key.public(),
            direct_addrs: vec![SocketAddr::new(
                IpAddr::V4(Ipv4Addr::new(192, 0, 2, 45)),
                8233,
            )],
            services: vec![ZakuraServiceId::discovery()],
            zakura_protocol_min: handshake.zakura_protocol_min,
            zakura_protocol_max: handshake.zakura_protocol_max,
            network_id: handshake.network_id,
            chain_id: handshake.chain_id,
            sequence: 1,
            expires_at_unix_secs: current_test_unix_secs().saturating_add(60),
        };
        Ok(ZakuraNodeRecord::sign(body, secret_key)?)
    }

    async fn complete_peer_side_discovery_exchange(
        peer_send: &FramedSend,
        peer_recv: &mut FramedRecv,
        peer_secret: &SecretKey,
        handshake: &ZakuraHandshakeConfig,
    ) -> Result<(), crate::BoxError> {
        let mut saw_hello = false;
        let mut saw_get_peers = false;
        let mut saw_get_services = false;
        while !(saw_hello && saw_get_peers && saw_get_services) {
            let frame = tokio::time::timeout(Duration::from_secs(2), peer_recv.recv())
                .await?
                .expect("discovery source sends exchange frames");
            match decode_discovery_frame(&frame)? {
                DiscoveryMessage::Hello { .. } => saw_hello = true,
                DiscoveryMessage::GetPeers { .. } => saw_get_peers = true,
                DiscoveryMessage::GetServices(_) => saw_get_services = true,
                DiscoveryMessage::Peers { .. } | DiscoveryMessage::Services(_) => {}
            }
        }

        peer_send
            .send(discovery_frame(DiscoveryMessage::Hello {
                record: signed_discovery_record(peer_secret, handshake)?,
            })?)
            .await?;
        peer_send
            .send(discovery_frame(DiscoveryMessage::Peers {
                records: Vec::new(),
            })?)
            .await?;
        let summary = DiscoveryServiceSummary {
            peer_exchange_slots_free: 1,
            max_records_per_response: 1,
            expected_disconnect_after_exchange: true,
        };
        peer_send
            .send(discovery_frame(DiscoveryMessage::Services(Services {
                node_id: peer_secret.public(),
                expires_at_unix_secs: u64::MAX,
                summaries: vec![ServiceSummaryEnvelope::discovery(&summary)?],
            }))?)
            .await?;

        Ok(())
    }

    async fn wait_for_discovery_inbound_peers(handle: &ZakuraDiscoveryHandle, expected: usize) {
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if handle.peer_snapshot().inbound_peers == expected {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("discovery peer snapshot reaches expected inbound count");
    }

    async fn advisory_backoff_after_empty_headers(
        fixture: &mut HeaderAdvisoryFixture,
    ) -> Result<bool, crate::BoxError> {
        let (send, _recv) = framed_channel(32);
        let session = HeaderSyncPeerSession::from_parts_with_direction(
            fixture.peer_id.clone(),
            ServicePeerDirection::Inbound,
            send,
            CancellationToken::new(),
        );
        fixture
            .header_sync
            .send(HeaderSyncEvent::PeerConnected(session))
            .await?;
        fixture
            .header_sync
            .send(HeaderSyncEvent::WireMessage {
                peer: fixture.peer_id.clone(),
                msg: HeaderSyncMessage::Status(HeaderSyncStatus {
                    tip_height: block::Height(1),
                    tip_hash: block::Hash([9; 32]),
                    anchor_height: block::Height(0),
                    max_headers_per_response: 1,
                    max_inflight_requests: 1,
                }),
            })
            .await?;

        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if let Some(HeaderSyncAction::SendMessage {
                    peer,
                    msg: HeaderSyncMessage::GetHeaders { .. },
                }) = fixture.header_actions.recv().await
                {
                    if peer == fixture.peer_id {
                        return;
                    }
                }
            }
        })
        .await
        .expect("header sync schedules a request before empty response");

        fixture
            .header_sync
            .send(HeaderSyncEvent::WireMessage {
                peer: fixture.peer_id.clone(),
                msg: HeaderSyncMessage::Headers {
                    headers: Vec::new(),
                    body_sizes: Vec::new(),
                },
            })
            .await?;
        tokio::time::sleep(Duration::from_millis(20)).await;

        Ok(fixture
            .header_sync
            .candidate_state()
            .backed_off_node_ids
            .contains(&fixture.peer_node_id))
    }

    #[tokio::test]
    async fn get_services_returns_local_first_party_discovery_summary(
    ) -> Result<(), crate::BoxError> {
        let (_connected_tx, connected_rx) = watch::channel(Vec::new());
        let handshake = ZakuraHandshakeConfig::for_network(&Network::Mainnet);
        let local_secret = SecretKey::from_bytes(&[21u8; 32]);
        let handle = ZakuraDiscoveryHandle::new(
            ZakuraDiscoveryLocalConfig {
                secret_key: local_secret.clone(),
                direct_addrs: Vec::new(),
                services: vec![ZakuraServiceId::discovery()],
                zakura_protocol_min: handshake.zakura_protocol_min,
                zakura_protocol_max: handshake.zakura_protocol_max,
                network_id: handshake.network_id,
                chain_id: handshake.chain_id,
                last_authored_sequence: None,
            },
            ZakuraDiscoveryConfig {
                peer_limits: ServicePeerLimits {
                    max_inbound_peers: 4,
                    ..ServicePeerLimits::default()
                },
                ..ZakuraDiscoveryConfig::default()
            },
            connected_rx,
        )?;
        let service = DiscoveryService::new(handle.clone());
        let peer_node_id = SecretKey::from_bytes(&[22u8; 32]).public();
        let peer_id = ZakuraPeerId::new(peer_node_id.as_bytes().to_vec())?;
        let (peer_send, service_recv) = framed_channel(8);
        let (service_send, mut peer_recv) = framed_channel(8);
        let streams = HashMap::from([(ZAKURA_STREAM_DISCOVERY, (service_recv, service_send))]);

        service.add_peer(Peer::new(
            peer_id,
            None,
            ZAKURA_CAP_DISCOVERY,
            streams,
            CancellationToken::new(),
        ));

        peer_send
            .send(Frame {
                message_type: DISCOVERY_FRAME_MESSAGE_TYPE,
                flags: 0,
                payload: DiscoveryMessage::GetServices(GetServices {
                    wanted_services: vec![ZakuraServiceId::discovery()],
                })
                .encode()?,
            })
            .await?;

        let services = tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                let frame = peer_recv.recv().await.expect("discovery stream stays open");
                let message = decode_discovery_frame(&frame).expect("outbound frame decodes");
                if let DiscoveryMessage::Services(services) = message {
                    return services;
                }
            }
        })
        .await
        .expect("service response is sent");

        assert_eq!(services.node_id, local_secret.public());
        assert_eq!(services.summaries.len(), 1);
        assert_eq!(
            services.summaries[0].service_id,
            ZakuraServiceId::discovery()
        );
        let summary = services.summaries[0]
            .decode_discovery()?
            .expect("discovery summary tag decodes");
        assert_eq!(summary.peer_exchange_slots_free, 3);
        assert!(summary.expected_disconnect_after_exchange);
        assert_eq!(
            summary.max_records_per_response,
            u16::try_from(MAX_DISCOVERY_RECORDS_PER_RESPONSE)
                .expect("record response cap fits in u16")
        );

        Ok(())
    }

    #[tokio::test]
    async fn get_services_returns_local_first_party_block_sync_summary(
    ) -> Result<(), crate::BoxError> {
        let (_connected_tx, connected_rx) = watch::channel(Vec::new());
        let handshake = ZakuraHandshakeConfig::for_network(&Network::Mainnet);
        let local_secret = SecretKey::from_bytes(&[24u8; 32]);
        let discovery_handle = ZakuraDiscoveryHandle::new(
            ZakuraDiscoveryLocalConfig {
                secret_key: local_secret.clone(),
                direct_addrs: Vec::new(),
                services: vec![ZakuraServiceId::discovery(), ZakuraServiceId::block_sync()],
                zakura_protocol_min: handshake.zakura_protocol_min,
                zakura_protocol_max: handshake.zakura_protocol_max,
                network_id: handshake.network_id,
                chain_id: handshake.chain_id,
                last_authored_sequence: None,
            },
            ZakuraDiscoveryConfig::default(),
            connected_rx,
        )?;
        let (header_sync, _header_actions, header_task) = spawn_test_header_sync()?;
        let (tip_tx, tip_rx) = watch::channel((block::Height(5), block::Hash([5; 32])));
        drop(tip_tx);
        let (block_sync, _block_actions, block_task) =
            spawn_block_sync_reactor(BlockSyncStartup::new(
                BlockSyncFrontiers {
                    finalized_height: block::Height(0),
                    verified_block_tip: block::Height(5),
                    verified_block_hash: block::Hash([5; 32]),
                },
                (block::Height(5), block::Hash([5; 32])),
                tip_rx,
                ZakuraBlockSyncConfig::default(),
            ));
        let service = DiscoveryService::with_sync_services(
            discovery_handle,
            header_sync,
            Some(block_sync.clone()),
        );
        let peer_node_id = SecretKey::from_bytes(&[25u8; 32]).public();
        let peer_id = ZakuraPeerId::new(peer_node_id.as_bytes().to_vec())?;
        let (peer_send, service_recv) = framed_channel(8);
        let (service_send, mut peer_recv) = framed_channel(8);
        let streams = HashMap::from([(ZAKURA_STREAM_DISCOVERY, (service_recv, service_send))]);

        service.add_peer(Peer::new(
            peer_id,
            None,
            ZAKURA_CAP_DISCOVERY | ZAKURA_CAP_BLOCK_SYNC,
            streams,
            CancellationToken::new(),
        ));

        peer_send
            .send(Frame {
                message_type: DISCOVERY_FRAME_MESSAGE_TYPE,
                flags: 0,
                payload: DiscoveryMessage::GetServices(GetServices {
                    wanted_services: vec![ZakuraServiceId::block_sync()],
                })
                .encode()?,
            })
            .await?;

        let services = tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                let frame = peer_recv.recv().await.expect("discovery stream stays open");
                let message = decode_discovery_frame(&frame).expect("outbound frame decodes");
                if let DiscoveryMessage::Services(services) = message {
                    return services;
                }
            }
        })
        .await
        .expect("service response is sent");

        assert_eq!(services.node_id, local_secret.public());
        assert_eq!(services.summaries.len(), 1);
        assert_eq!(
            services.summaries[0].service_id,
            ZakuraServiceId::block_sync()
        );
        let summary = services.summaries[0]
            .decode_block_sync()?
            .expect("block summary tag decodes");
        assert_eq!(summary.servable_low, block::Height(0));
        assert_eq!(summary.servable_high, block::Height(5));
        assert_eq!(summary.tip_hash, block::Hash([5; 32]));
        assert_eq!(
            usize::from(summary.free_slots),
            block_sync.peer_snapshot().inbound_slots_free
        );
        assert_eq!(
            summary.max_blocks_per_response,
            ZakuraBlockSyncConfig::default().advertised_max_blocks_per_response()
        );
        assert_eq!(summary.max_response_bytes, MAX_BS_RESPONSE_BYTES);

        header_task.abort();
        block_task.abort();
        Ok(())
    }

    #[tokio::test]
    async fn inbound_services_updates_first_party_live_summary_cache() -> Result<(), crate::BoxError>
    {
        let (connected_tx, connected_rx) = watch::channel(Vec::new());
        let handshake = ZakuraHandshakeConfig::for_network(&Network::Mainnet);
        let local_secret = SecretKey::from_bytes(&[23u8; 32]);
        let handle = ZakuraDiscoveryHandle::new(
            ZakuraDiscoveryLocalConfig {
                secret_key: local_secret,
                direct_addrs: Vec::new(),
                services: vec![ZakuraServiceId::discovery()],
                zakura_protocol_min: handshake.zakura_protocol_min,
                zakura_protocol_max: handshake.zakura_protocol_max,
                network_id: handshake.network_id,
                chain_id: handshake.chain_id,
                last_authored_sequence: None,
            },
            ZakuraDiscoveryConfig::default(),
            connected_rx,
        )?;
        let service = DiscoveryService::new(handle.clone());
        let peer_node_id = SecretKey::from_bytes(&[24u8; 32]).public();
        let peer_id = ZakuraPeerId::new(peer_node_id.as_bytes().to_vec())?;
        connected_tx.send_replace(vec![peer_id.clone()]);

        let (peer_send, service_recv) = framed_channel(8);
        let (service_send, _peer_recv) = framed_channel(8);
        let streams = HashMap::from([(ZAKURA_STREAM_DISCOVERY, (service_recv, service_send))]);

        service.add_peer(Peer::new(
            peer_id,
            None,
            ZAKURA_CAP_DISCOVERY,
            streams,
            CancellationToken::new(),
        ));

        let summary = DiscoveryServiceSummary {
            peer_exchange_slots_free: 7,
            max_records_per_response: 11,
            expected_disconnect_after_exchange: false,
        };
        peer_send
            .send(Frame {
                message_type: DISCOVERY_FRAME_MESSAGE_TYPE,
                flags: 0,
                payload: DiscoveryMessage::Services(Services {
                    node_id: peer_node_id,
                    expires_at_unix_secs: u64::MAX,
                    summaries: vec![ServiceSummaryEnvelope::discovery(&summary)?],
                })
                .encode()?,
            })
            .await?;

        let cached = tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if let Some(cached) = handle.live_service_summaries(peer_node_id).await {
                    if !cached.is_empty() {
                        return cached;
                    }
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("inbound SERVICES is imported");

        assert_eq!(cached.len(), 1);
        assert_eq!(
            cached[0].summary,
            ZakuraLiveServiceSummary::Discovery(summary)
        );

        Ok(())
    }

    #[tokio::test]
    async fn first_party_header_services_emit_header_sync_advisory() -> Result<(), crate::BoxError>
    {
        let mut fixture = spawn_header_advisory_fixture(25)?;
        let summary = header_summary(block::Height(10));

        send_discovery_message(
            &fixture,
            DiscoveryMessage::Services(Services {
                node_id: fixture.peer_node_id,
                expires_at_unix_secs: u64::MAX,
                summaries: vec![ServiceSummaryEnvelope::header_sync(&summary)?],
            }),
        )
        .await?;

        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if let Some(cached) = fixture
                    .discovery_handle
                    .live_service_summaries(fixture.peer_node_id)
                    .await
                {
                    if cached.iter().any(|cached_summary| {
                        cached_summary.summary == ZakuraLiveServiceSummary::HeaderSync(summary)
                    }) {
                        return;
                    }
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("first-party header summary is cached");

        assert!(
            advisory_backoff_after_empty_headers(&mut fixture).await?,
            "first-party header SERVICES should emit a header-sync advisory event"
        );

        Ok(())
    }

    #[tokio::test]
    async fn mismatched_services_node_id_does_not_emit_header_sync_advisory(
    ) -> Result<(), crate::BoxError> {
        let mut fixture = spawn_header_advisory_fixture(26)?;
        let claimed_node_id = SecretKey::from_bytes(&[27u8; 32]).public();
        let summary = header_summary(block::Height(10));

        send_discovery_message(
            &fixture,
            DiscoveryMessage::Services(Services {
                node_id: claimed_node_id,
                expires_at_unix_secs: u64::MAX,
                summaries: vec![ServiceSummaryEnvelope::header_sync(&summary)?],
            }),
        )
        .await?;
        tokio::time::sleep(Duration::from_millis(20)).await;

        assert_eq!(
            fixture
                .discovery_handle
                .live_service_summaries(fixture.peer_node_id)
                .await,
            None
        );
        assert_eq!(
            fixture
                .discovery_handle
                .live_service_summaries(claimed_node_id)
                .await,
            None
        );
        assert!(
            !advisory_backoff_after_empty_headers(&mut fixture).await?,
            "mismatched SERVICES node id must not emit a header-sync advisory event"
        );

        Ok(())
    }

    #[tokio::test]
    async fn peers_response_does_not_emit_header_sync_advisory() -> Result<(), crate::BoxError> {
        let mut fixture = spawn_header_advisory_fixture(28)?;
        let handshake = ZakuraHandshakeConfig::for_network(&Network::Mainnet);
        let record_secret = SecretKey::from_bytes(&[29u8; 32]);
        let record = signed_header_sync_record(&record_secret, &handshake)?;

        send_discovery_message(
            &fixture,
            DiscoveryMessage::Peers {
                records: vec![record.clone()],
            },
        )
        .await?;
        tokio::time::sleep(Duration::from_millis(20)).await;

        assert_eq!(
            fixture
                .discovery_handle
                .live_service_summaries(record.body.node_id)
                .await,
            None
        );
        assert!(
            !advisory_backoff_after_empty_headers(&mut fixture).await?,
            "PEERS/gossiped records must not emit live header-sync advisory events"
        );

        Ok(())
    }

    #[tokio::test]
    async fn discovery_only_short_lived_exchange_closes_connection_and_backs_off(
    ) -> Result<(), crate::BoxError> {
        let (connected_tx, connected_rx) = watch::channel(Vec::new());
        let handshake = ZakuraHandshakeConfig::for_network(&Network::Mainnet);
        let local_secret = SecretKey::from_bytes(&[40u8; 32]);
        let handle = ZakuraDiscoveryHandle::new(
            ZakuraDiscoveryLocalConfig {
                secret_key: local_secret,
                direct_addrs: Vec::new(),
                services: vec![ZakuraServiceId::discovery()],
                zakura_protocol_min: handshake.zakura_protocol_min,
                zakura_protocol_max: handshake.zakura_protocol_max,
                network_id: handshake.network_id,
                chain_id: handshake.chain_id,
                last_authored_sequence: None,
            },
            ZakuraDiscoveryConfig::default(),
            connected_rx,
        )?;
        let service = DiscoveryService::new(handle.clone());
        let peer_secret = SecretKey::from_bytes(&[41u8; 32]);
        let peer_node_id = peer_secret.public();
        let peer_id = ZakuraPeerId::new(peer_node_id.as_bytes().to_vec())?;
        connected_tx.send_replace(vec![peer_id.clone()]);

        let connection_cancel = CancellationToken::new();
        let (peer_send, service_recv) = framed_channel(16);
        let (service_send, mut peer_recv) = framed_channel(16);
        let streams = HashMap::from([(ZAKURA_STREAM_DISCOVERY, (service_recv, service_send))]);

        service.add_peer(Peer::new(
            peer_id,
            None,
            ZAKURA_CAP_DISCOVERY,
            streams,
            connection_cancel.clone(),
        ));

        wait_for_discovery_inbound_peers(&handle, 1).await;
        complete_peer_side_discovery_exchange(&peer_send, &mut peer_recv, &peer_secret, &handshake)
            .await?;
        tokio::time::timeout(Duration::from_secs(2), connection_cancel.cancelled())
            .await
            .expect("discovery-only exchange closes the shared connection");
        wait_for_discovery_inbound_peers(&handle, 0).await;

        connected_tx.send_replace(Vec::new());
        assert!(handle
            .dial_candidates(&[ZakuraServiceId::discovery()], &[])
            .await
            .is_empty());

        Ok(())
    }

    #[tokio::test]
    async fn discovery_short_lived_exchange_keeps_header_sync_connection(
    ) -> Result<(), crate::BoxError> {
        let (connected_tx, connected_rx) = watch::channel(Vec::new());
        let handshake = ZakuraHandshakeConfig::for_network(&Network::Mainnet);
        let local_secret = SecretKey::from_bytes(&[42u8; 32]);
        let discovery_handle = ZakuraDiscoveryHandle::new(
            ZakuraDiscoveryLocalConfig {
                secret_key: local_secret,
                direct_addrs: Vec::new(),
                services: vec![ZakuraServiceId::discovery()],
                zakura_protocol_min: handshake.zakura_protocol_min,
                zakura_protocol_max: handshake.zakura_protocol_max,
                network_id: handshake.network_id,
                chain_id: handshake.chain_id,
                last_authored_sequence: None,
            },
            ZakuraDiscoveryConfig::default(),
            connected_rx,
        )?;
        let (header_sync, _header_actions, header_task) = spawn_test_header_sync()?;
        let service = DiscoveryService::with_sync_services(
            discovery_handle.clone(),
            header_sync.clone(),
            None,
        );
        let peer_secret = SecretKey::from_bytes(&[43u8; 32]);
        let peer_node_id = peer_secret.public();
        let peer_id = ZakuraPeerId::new(peer_node_id.as_bytes().to_vec())?;
        connected_tx.send_replace(vec![peer_id.clone()]);

        let (header_send, _header_recv) = framed_channel(8);
        let header_session = HeaderSyncPeerSession::from_parts_with_direction(
            peer_id.clone(),
            ServicePeerDirection::Inbound,
            header_send,
            CancellationToken::new(),
        );
        header_sync
            .send(HeaderSyncEvent::PeerConnected(header_session))
            .await?;
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if header_sync
                    .candidate_state()
                    .admitted_node_ids
                    .contains(&peer_node_id)
                {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("header sync admits the peer");

        let connection_cancel = CancellationToken::new();
        let (peer_send, service_recv) = framed_channel(16);
        let (service_send, mut peer_recv) = framed_channel(16);
        let streams = HashMap::from([(ZAKURA_STREAM_DISCOVERY, (service_recv, service_send))]);
        service.add_peer(Peer::new(
            peer_id,
            None,
            ZAKURA_CAP_DISCOVERY | ZAKURA_CAP_HEADER_SYNC,
            streams,
            connection_cancel.clone(),
        ));

        wait_for_discovery_inbound_peers(&discovery_handle, 1).await;
        complete_peer_side_discovery_exchange(&peer_send, &mut peer_recv, &peer_secret, &handshake)
            .await?;
        wait_for_discovery_inbound_peers(&discovery_handle, 0).await;
        assert_eq!(header_sync.peer_snapshot().inbound_peers, 1);
        assert!(
            tokio::time::timeout(Duration::from_millis(100), connection_cancel.cancelled())
                .await
                .is_err(),
            "discovery releases only its own session while header sync owns the connection"
        );

        header_task.abort();
        Ok(())
    }
}
