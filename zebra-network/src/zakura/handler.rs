//! Zakura P2P v2 endpoint, protocol handler, and bounded connection serving.

use std::{
    collections::{HashMap, HashSet},
    future,
    io::{Cursor, Read},
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, SocketAddrV4, SocketAddrV6},
    path::PathBuf,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex as StdMutex,
    },
    time::Duration,
};

use byteorder::{LittleEndian, ReadBytesExt, WriteBytesExt};
use iroh::Watcher as _;
use iroh::{
    endpoint::{
        Connection, ConnectionType, Endpoint, RecvStream, SendStream, TransportConfig, VarInt,
    },
    protocol::{AcceptError, ProtocolHandler, Router},
    NodeAddr, NodeId, SecretKey,
};
use rand::{rngs::OsRng, RngCore};
use thiserror::Error;
use tokio::{
    sync::{mpsc, oneshot, watch, Mutex, OwnedSemaphorePermit, Semaphore},
    task::{AbortHandle, JoinHandle, JoinSet},
    time::{timeout, Instant},
};
use tokio_util::sync::CancellationToken;
use zebra_chain::{
    block::{self, Block, CountedHeader},
    parameters::Network,
    serialization::{CompactSizeMessage, ZcashDeserialize, MAX_HEADERS_PER_MESSAGE},
    transaction::Transaction,
};

use super::discovery::{native_dial_supervised, spawn_native_bootstrap_dialer, RedialPolicy};
use super::{
    trace::{
        peer_label as trace_peer_label, reject_reason_label, ZakuraTrace, CONN_TABLE,
        HANDSHAKE_TABLE, RATELIMIT_TABLE, STREAM_TABLE,
    },
    ZakuraTraceEvent,
};
use crate::{
    protocol::external::InventoryHash,
    zakura::{
        direct_endpoint_builder, drive_header_sync_actions, spawn_block_sync_reactor,
        spawn_header_sync_reactor, BlockSyncAction, BlockSyncFrontiers, BlockSyncHandle,
        BlockSyncService, BlockSyncStartup, Clock, Frame, FramedRecv, FramedSend, Frontier,
        FrontierChange, FrontierUpdate, HeaderSyncAction, HeaderSyncFrontiers,
        HeaderSyncPassthroughService, HeaderSyncService, HeaderSyncStartup, Peer, RealClock,
        Service, ServicePeerDirection, ServiceRegistry, ServiceStream, SinkReject, Stream,
        StreamMode, StreamPrelude, ZakuraAcceptedLimits, ZakuraBlockSyncConfig, ZakuraControlAck,
        ZakuraControlHello, ZakuraControlRole, ZakuraControlValidation, ZakuraHandshakeConfig,
        ZakuraHandshakePath, ZakuraHeaderSyncConfig, ZakuraInitialLimits, ZakuraLimits,
        ZakuraPeerId, ZakuraPeerSupervisor, ZakuraProtocolError, ZakuraRejectReason,
        ZakuraSyncExchange, ZakuraUpgradeOutcome, CONTROL_ACK_MAGIC, CONTROL_HELLO_MAGIC,
        CONTROL_VERSION, FRAME_HEADER_BYTES, LOCAL_MAX_CONTROL_FRAME_BYTES, MAX_BS_FRAME_BYTES,
        MAX_CONTROL_PAYLOAD_BYTES, MAX_HS_MESSAGE_BYTES, P2P_V2_ALPN, STREAM_PRELUDE_MAGIC,
        TRANSCRIPT_HASH_BYTES, ZAKURA_HEADER_SYNC_STREAM_VERSION, ZAKURA_PROTOCOL_VERSION_1,
        ZAKURA_STREAM_BLOCK_SYNC, ZAKURA_STREAM_HEADER_SYNC,
    },
};
use crate::{BoxError, Config, MAX_TX_INV_IN_SENT_MESSAGE};

/// Conservative default for total Zakura connections when P2P v2 is enabled.
pub const DEFAULT_ZAKURA_MAX_CONNECTIONS: usize = 32;
/// Conservative default for simultaneous Zakura control handshakes.
pub const DEFAULT_ZAKURA_MAX_PENDING_HANDSHAKES: usize = 8;
/// Conservative default for stream-open churn per connection.
pub const DEFAULT_ZAKURA_STREAM_OPEN_RATE_PER_SECOND: u32 = 16;
/// Per-kind inbound message rate per connection.
///
/// This is a generous universal cap: block-sync legitimately delivers
/// hundreds of solicited bodies per second in bursts, so a low limit
/// starves sync. Exceeding it is treated as misbehavior and disconnects the
/// peer (we never silently drop a solicited frame -- a dropped block body is
/// a permanent gap on a reliable stream). Longer term this should be split
/// per message type (some unbounded, some near-one-shot) rather than a single
/// universal value.
pub const DEFAULT_ZAKURA_MESSAGE_RATE_PER_SECOND: u32 = 2048;
/// Default native Zakura QUIC listen address.
pub const DEFAULT_ZAKURA_LISTEN_ADDR: SocketAddr =
    SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, 8234));
/// Default maximum bytes read before the peer's stream prelude is decoded.
pub const DEFAULT_ZAKURA_PRELUDE_TIMEOUT: Duration = Duration::from_secs(3);
/// Default timeout for one control-handshake read or write.
pub const DEFAULT_ZAKURA_CONTROL_TIMEOUT: Duration = Duration::from_secs(10);
/// QUIC idle timeout used by Zakura endpoints.
///
/// This also bounds the application-level idle reaper that closes a connection
/// after a quiet period. A gossip connection is legitimately quiet between
/// blocks (minutes apart on mainnet), so a short timeout would tear down healthy
/// peers and force constant re-dials; the keepalive interval keeps the transport
/// live well within this window.
pub const DEFAULT_ZAKURA_QUIC_IDLE_TIMEOUT: Duration = Duration::from_secs(150);
/// QUIC keepalive interval used by Zakura endpoints.
pub const DEFAULT_ZAKURA_KEEP_ALIVE_INTERVAL: Duration = Duration::from_secs(10);
/// Minimum age of an incumbent Zakura connection before a duplicate connection
/// for the same identity is allowed to evict it.
///
/// Duplicates can be ordinary redial races while the incumbent is healthy and
/// actively serving block sync. Evicting those incumbents drops in-flight body
/// ranges and makes a fresh sync repeatedly re-download the same windows. Keep
/// the incumbent through the transport idle timeout; genuinely dead connections
/// are reaped by QUIC idle cleanup, and duplicate redials after that point can
/// reclaim the slot without disturbing active transfers.
pub const ZAKURA_DUPLICATE_EVICT_MIN_AGE: Duration = Duration::from_secs(300);
/// QUIC stream receive window used by Zakura endpoints.
pub const DEFAULT_ZAKURA_STREAM_RECEIVE_WINDOW: u32 = 512 * 1024;
/// QUIC connection receive window used by Zakura endpoints.
pub const DEFAULT_ZAKURA_RECEIVE_WINDOW: u32 = 2 * 1024 * 1024;
/// QUIC send window used by Zakura endpoints.
pub const DEFAULT_ZAKURA_SEND_WINDOW: u64 = 2 * 1024 * 1024;
/// Initial backoff before re-dialing a configured Zakura bootstrap peer.
pub const DEFAULT_ZAKURA_REDIAL_INITIAL_BACKOFF: Duration = Duration::from_secs(1);
/// Maximum backoff between re-dials of a configured Zakura bootstrap peer.
pub const DEFAULT_ZAKURA_REDIAL_MAX_BACKOFF: Duration = Duration::from_secs(30);
const CONTROL_LENGTH_BYTES: usize = 4;
const STREAM_PRELUDE_FIXED_BYTES: usize = 4 + 2 + 2 + 1;
const STREAM_PRELUDE_REQUEST_ID_FLAG_OFFSET: usize = STREAM_PRELUDE_FIXED_BYTES - 1;
const STREAM_PRELUDE_REQUEST_ID_BYTES: usize = 8;
const STREAM_PRELUDE_CAP_BYTES: usize = 4;
const STREAM_WORKER_DRAIN_TIMEOUT: Duration = Duration::from_secs(1);
const OUTBOUND_STREAM_WRITE_TIMEOUT: Duration = Duration::from_secs(10);
const OUTBOUND_REQUEST_RESPONSE_TIMEOUT: Duration = Duration::from_secs(30);
// Mirrors the legacy gossip compatibility protocol. Compile-time assertions
// below keep this transport-side budget validator pinned to the codec constants.
const LEGACY_GOSSIP_STREAM_KIND: u16 = 2;
const LEGACY_REQUEST_STREAM_KIND: u16 = 3;
const DISCOVERY_STREAM_KIND: u16 = 4;
const HEADER_SYNC_STREAM_KIND: u16 = ZAKURA_STREAM_HEADER_SYNC;
const LEGACY_REQUEST_BLOCKS_BY_HASH: u16 = 3;
const LEGACY_REQUEST_TRANSACTIONS_BY_ID: u16 = 4;
const LEGACY_RESPONSE_BLOCK: u16 = 5;
const LEGACY_RESPONSE_TRANSACTION: u16 = 6;
const LEGACY_RESPONSE_MISSING_BLOCKS: u16 = 7;
const LEGACY_RESPONSE_MISSING_TRANSACTIONS: u16 = 8;
const LEGACY_REQUEST_FIND_BLOCKS: u16 = 9;
const LEGACY_REQUEST_FIND_HEADERS: u16 = 10;
const LEGACY_REQUEST_MEMPOOL_TRANSACTION_IDS: u16 = 11;
const LEGACY_REQUEST_PING: u16 = 12;
const LEGACY_REQUEST_PUSH_TRANSACTION: u16 = 13;
const LEGACY_RESPONSE_BLOCK_HASHES: u16 = 14;
const LEGACY_RESPONSE_BLOCK_HEADERS: u16 = 15;
const LEGACY_RESPONSE_TRANSACTION_IDS: u16 = 16;
const LEGACY_RESPONSE_PONG: u16 = 17;
const LEGACY_RESPONSE_NIL: u16 = 18;
const LEGACY_RESPONSE_REQUEST_ID_BYTES: usize = 8;
const LEGACY_RESPONSE_CHUNK_HEADER_BYTES: usize = LEGACY_RESPONSE_REQUEST_ID_BYTES + 1;
const LEGACY_COMPACT_SIZE_PREFIX_BYTES: usize = 9;
const LEGACY_BLOCK_HASH_BYTES: usize = 32;
const LEGACY_INVENTORY_HASH_BYTES: usize = 36;
const LEGACY_RESPONSE_MAX_FRAMES_PER_ITEM: usize = 8;
/// Maximum cumulative response payload bytes the requester will retain in the
/// accepted-frame `Vec` before `decode_response` consumes it.
///
/// `LegacyResponseBudget::from_request` otherwise derives the per-response byte
/// budget for Blocks/Transactions as `item_count * max_message_bytes`, so a
/// request naming the protocol-max inventory count (`MAX_TX_INV_IN_SENT_MESSAGE`)
/// would let a hostile responder fill ~`25_000 * MAX_PROTOCOL_MESSAGE_LEN` (tens
/// of GiB) of validated frames before any decode begins. The inbound responder
/// already caps a single response's cumulative payload at the same value
/// (`legacy_gossip::LEGACY_RESPONSE_MAX_AGGREGATE_BYTES`), so an honest peer
/// never sends more than this; clamping the requester budget to the same
/// operational aggregate is the symmetric requester-side mirror.
const LEGACY_RESPONSE_MAX_AGGREGATE_BYTES: usize =
    8 * zebra_chain::serialization::MAX_PROTOCOL_MESSAGE_LEN;
const _: () = assert!(LEGACY_GOSSIP_STREAM_KIND == super::legacy_gossip::ZAKURA_STREAM_GOSSIP);
const _: () =
    assert!(LEGACY_REQUEST_STREAM_KIND == super::legacy_gossip::ZAKURA_STREAM_LEGACY_REQUESTS);
const _: () = assert!(DISCOVERY_STREAM_KIND == super::discovery::ZAKURA_STREAM_DISCOVERY);
const _: () = assert!(HEADER_SYNC_STREAM_KIND == super::header_sync::ZAKURA_STREAM_HEADER_SYNC);
const _: () = assert!(ZAKURA_STREAM_VERSION_2 == ZAKURA_HEADER_SYNC_STREAM_VERSION);
const _: () =
    assert!(LEGACY_REQUEST_BLOCKS_BY_HASH == super::legacy_gossip::MSG_REQUEST_BLOCKS_BY_HASH);
const _: () = assert!(
    LEGACY_REQUEST_TRANSACTIONS_BY_ID == super::legacy_gossip::MSG_REQUEST_TRANSACTIONS_BY_ID
);
const _: () = assert!(LEGACY_RESPONSE_BLOCK == super::legacy_gossip::MSG_RESPONSE_BLOCK);
const _: () =
    assert!(LEGACY_RESPONSE_TRANSACTION == super::legacy_gossip::MSG_RESPONSE_TRANSACTION);
const _: () =
    assert!(LEGACY_RESPONSE_MISSING_BLOCKS == super::legacy_gossip::MSG_RESPONSE_MISSING_BLOCKS);
const _: () = assert!(
    LEGACY_RESPONSE_MISSING_TRANSACTIONS == super::legacy_gossip::MSG_RESPONSE_MISSING_TRANSACTIONS
);
const _: () = assert!(LEGACY_REQUEST_FIND_BLOCKS == super::legacy_gossip::MSG_REQUEST_FIND_BLOCKS);
const _: () =
    assert!(LEGACY_REQUEST_FIND_HEADERS == super::legacy_gossip::MSG_REQUEST_FIND_HEADERS);
const _: () = assert!(
    LEGACY_REQUEST_MEMPOOL_TRANSACTION_IDS
        == super::legacy_gossip::MSG_REQUEST_MEMPOOL_TRANSACTION_IDS
);
const _: () = assert!(LEGACY_REQUEST_PING == super::legacy_gossip::MSG_REQUEST_PING);
const _: () =
    assert!(LEGACY_REQUEST_PUSH_TRANSACTION == super::legacy_gossip::MSG_REQUEST_PUSH_TRANSACTION);
const _: () =
    assert!(LEGACY_RESPONSE_BLOCK_HASHES == super::legacy_gossip::MSG_RESPONSE_BLOCK_HASHES);
const _: () =
    assert!(LEGACY_RESPONSE_BLOCK_HEADERS == super::legacy_gossip::MSG_RESPONSE_BLOCK_HEADERS);
const _: () =
    assert!(LEGACY_RESPONSE_TRANSACTION_IDS == super::legacy_gossip::MSG_RESPONSE_TRANSACTION_IDS);
const _: () = assert!(LEGACY_RESPONSE_PONG == super::legacy_gossip::MSG_RESPONSE_PONG);
const _: () = assert!(LEGACY_RESPONSE_NIL == super::legacy_gossip::MSG_RESPONSE_NIL);
const ZAKURA_CLOSE_NEUTRAL: u32 = 0;
const ZAKURA_CLOSE_RESOURCE: u32 = 1;
const ZAKURA_CLOSE_BAD_PRELUDE: u32 = 2;
const ZAKURA_CLOSE_RATE_LIMIT: u32 = 3;
const ZAKURA_CLOSE_OVERSIZE: u32 = 4;
/// Stream reset code for a prelude naming an unknown stream kind or an
/// unsupported version of a known kind. Bounded, peer-visible, and distinct
/// from the malformed-prelude code so peers can tell a parse failure from an
/// unsupported-but-well-formed stream.
const ZAKURA_CLOSE_UNKNOWN_STREAM: u32 = 5;

/// Native Zakura endpoint and handler configuration.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct ZakuraConfig {
    /// Native Zakura bootstrap peers as `node_id@direct_addr`.
    ///
    /// Native bootstrap uses direct iroh addresses only because relays and discovery
    /// are disabled for Zakura v1. Each entry must contain the peer's 32-byte
    /// iroh node id and a directly reachable socket address.
    pub bootstrap_peers: Vec<String>,
    /// Address the native Zakura QUIC endpoint binds to.
    ///
    /// Defaults to `0.0.0.0:8234`, giving the node a stable, advertisable
    /// Zakura endpoint so other nodes can list it in their
    /// [`bootstrap_peers`](Self::bootstrap_peers). If code constructs this as
    /// `None`, the endpoint binds an OS-assigned ephemeral port on loopback
    /// only (`127.0.0.1` and `::1`), which is fine for a node that only dials
    /// out and keeps the experimental native P2P_V2_ALPN surface off all
    /// non-loopback interfaces.
    pub listen_addr: Option<SocketAddr>,
    /// Total concurrent Zakura connections, inbound plus outbound.
    pub max_connections: usize,
    /// Connections concurrently running the control handshake.
    pub max_pending_handshakes: usize,
    /// New streams per second admitted per connection after a valid prelude.
    pub stream_open_rate_per_second: u32,
    /// Messages per second admitted per stream kind on a connection.
    pub message_rate_per_second: u32,
    /// Optional directory for structured Zakura JSONL trace tables.
    ///
    /// When unset, Zakura trace emission is disabled. When set, the native
    /// Zakura endpoint writes the production trace schema into this directory.
    pub trace_dir: Option<PathBuf>,
    /// Native stream-5 header-sync wire settings.
    pub header_sync: ZakuraHeaderSyncConfig,
    /// Native stream-6 block-sync wire, scheduling, serving, and rollout settings.
    pub block_sync: ZakuraBlockSyncConfig,
}

impl Default for ZakuraConfig {
    fn default() -> Self {
        Self {
            bootstrap_peers: Vec::new(),
            listen_addr: Some(DEFAULT_ZAKURA_LISTEN_ADDR),
            max_connections: DEFAULT_ZAKURA_MAX_CONNECTIONS,
            max_pending_handshakes: DEFAULT_ZAKURA_MAX_PENDING_HANDSHAKES,
            stream_open_rate_per_second: DEFAULT_ZAKURA_STREAM_OPEN_RATE_PER_SECOND,
            message_rate_per_second: DEFAULT_ZAKURA_MESSAGE_RATE_PER_SECOND,
            trace_dir: None,
            header_sync: ZakuraHeaderSyncConfig::default(),
            block_sync: ZakuraBlockSyncConfig::default(),
        }
    }
}

/// Hard local ceilings enforced by the Zakura endpoint and handler.
#[derive(Clone, Debug)]
pub struct ZakuraLocalLimits {
    /// Total concurrent Zakura connection cap.
    pub max_connections: usize,
    /// Concurrent control handshakes cap.
    pub max_pending_handshakes: usize,
    /// Per-connection QUIC/app idle timeout.
    pub quic_idle_timeout: Duration,
    /// QUIC keepalive interval.
    pub keep_alive_interval: Duration,
    /// Stream prelude read timeout.
    pub prelude_timeout: Duration,
    /// Control handshake read/write timeout.
    pub control_timeout: Duration,
    /// Per-connection stream-open rate.
    pub stream_open_rate_per_second: u32,
    /// Per-stream-kind message rate.
    pub message_rate_per_second: u32,
    /// Maximum frame bytes accepted locally.
    pub max_frame_bytes: u32,
    /// Maximum reassembled message bytes accepted locally.
    pub max_message_bytes: u32,
    /// Maximum concurrent admitted streams per connection.
    pub max_open_streams: u16,
    /// Maximum inbound queue depth per stream kind.
    pub max_inbound_queue_depth: u16,
}

impl ZakuraLocalLimits {
    /// Build local handler limits from network configuration and handshake policy.
    pub fn from_config(config: &Config) -> Self {
        let handshake = ZakuraHandshakeConfig::for_network(&config.network);
        Self {
            max_connections: config.zakura.max_connections.max(1),
            max_pending_handshakes: config.zakura.max_pending_handshakes.max(1),
            quic_idle_timeout: DEFAULT_ZAKURA_QUIC_IDLE_TIMEOUT,
            keep_alive_interval: DEFAULT_ZAKURA_KEEP_ALIVE_INTERVAL,
            prelude_timeout: DEFAULT_ZAKURA_PRELUDE_TIMEOUT,
            control_timeout: DEFAULT_ZAKURA_CONTROL_TIMEOUT,
            stream_open_rate_per_second: config.zakura.stream_open_rate_per_second.max(1),
            message_rate_per_second: config.zakura.message_rate_per_second.max(1),
            max_frame_bytes: handshake.max_message_bytes,
            max_message_bytes: handshake.max_message_bytes,
            max_open_streams: handshake.max_open_streams,
            max_inbound_queue_depth: handshake.max_inbound_queue_depth,
        }
    }

    /// Clamp peer-negotiated limits to local hard ceilings.
    pub fn clamp(&self, negotiated: &ZakuraAcceptedLimits) -> ZakuraConnectionLimits {
        let max_open_streams = negotiated
            .max_open_streams
            .min(self.max_open_streams)
            .max(1);
        let idle_timeout = Duration::from_millis(
            u64::from(negotiated.idle_timeout_millis)
                .min(self.quic_idle_timeout.as_millis().saturating_sub(1) as u64)
                .max(1),
        );

        ZakuraConnectionLimits {
            max_frame_bytes: negotiated.max_frame_bytes.min(self.max_frame_bytes).max(1),
            max_message_bytes: negotiated
                .max_message_bytes
                .min(self.max_message_bytes)
                .max(1),
            max_open_streams,
            max_inbound_queue_depth: negotiated
                .max_inbound_queue_depth
                .min(self.max_inbound_queue_depth)
                .max(1),
            idle_timeout,
            prelude_timeout: self.prelude_timeout,
            control_timeout: self.control_timeout,
            stream_open_rate_per_second: self.stream_open_rate_per_second,
            message_rate_per_second: self.message_rate_per_second,
        }
    }

    /// Returns the initial limits advertised in a native control hello.
    pub fn initial_limits(&self) -> ZakuraInitialLimits {
        ZakuraLimits {
            max_frame_bytes: self.max_frame_bytes,
            max_message_bytes: self.max_message_bytes,
            max_open_streams: self.max_open_streams,
            max_inbound_queue_depth: self.max_inbound_queue_depth,
            idle_timeout_millis: self.quic_idle_timeout.as_millis().saturating_sub(1) as u32,
        }
    }

    /// Returns the QUIC transport config matching these local limits.
    pub fn transport_config(&self) -> TransportConfig {
        let mut transport = TransportConfig::default();
        transport
            .max_concurrent_bidi_streams(VarInt::from_u32(u32::from(self.max_open_streams.min(64))))
            .max_concurrent_uni_streams(VarInt::from_u32(0))
            .stream_receive_window(VarInt::from_u32(DEFAULT_ZAKURA_STREAM_RECEIVE_WINDOW))
            .receive_window(VarInt::from_u32(DEFAULT_ZAKURA_RECEIVE_WINDOW))
            .send_window(DEFAULT_ZAKURA_SEND_WINDOW)
            .max_idle_timeout(Some(
                self.quic_idle_timeout
                    .try_into()
                    .expect("default Zakura idle timeout is a valid QUIC idle timeout"),
            ))
            .keep_alive_interval(Some(self.keep_alive_interval))
            .datagram_receive_buffer_size(None)
            .datagram_send_buffer_size(0);
        transport
    }
}

/// Per-connection limits after negotiation and clamping.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct ZakuraConnectionLimits {
    /// Maximum encoded frame bytes.
    pub max_frame_bytes: u32,
    /// Maximum reassembled message bytes.
    pub max_message_bytes: u32,
    /// Maximum concurrent streams.
    pub max_open_streams: u16,
    /// Bounded inbound queue depth.
    pub max_inbound_queue_depth: u16,
    /// Application idle timeout.
    pub idle_timeout: Duration,
    /// Stream prelude read timeout.
    pub prelude_timeout: Duration,
    /// Control handshake timeout.
    pub control_timeout: Duration,
    /// Per-connection stream-open rate.
    pub stream_open_rate_per_second: u32,
    /// Per-stream-kind message rate.
    pub message_rate_per_second: u32,
}

/// Running Zakura endpoint owned by `zebra-network`/`zebrad` startup.
#[derive(Debug, Clone)]
pub struct ZakuraEndpoint {
    router: Router,
    supervisor: ZakuraSupervisorHandle,
    handler: ZakuraProtocolHandler,
    header_sync: Option<super::HeaderSyncHandle>,
    block_sync: Option<BlockSyncHandle>,
    sync_frontier: Option<ZakuraSyncExchange>,
    header_sync_tasks: Option<Arc<HeaderSyncBackgroundTasks>>,
    header_sync_actions: Option<Arc<Mutex<Option<mpsc::Receiver<HeaderSyncAction>>>>>,
    block_sync_actions: Option<Arc<Mutex<Option<mpsc::Receiver<BlockSyncAction>>>>>,
    /// Maintained native dials started by the legacy->Zakura upgrade hand-off,
    /// keyed by the advertised peer id. The [`AbortHandle`] lets a failed
    /// hand-off cancel its maintain-forever dial instead of leaking it; see
    /// [`Self::ensure_upgrade_native_dial`] and [`Self::cancel_upgrade_native_dial`].
    upgrade_dials: Arc<StdMutex<HashMap<ZakuraPeerId, AbortHandle>>>,
}

#[derive(Debug)]
struct HeaderSyncBackgroundTasks {
    shutdown: CancellationToken,
    tasks: Mutex<Vec<JoinHandle<()>>>,
}

/// Durable state facts required before attaching the production header-sync driver.
#[derive(Clone, Debug)]
pub struct ZakuraHeaderSyncDriverStartup {
    /// Durable state frontiers loaded at node startup.
    pub frontiers: HeaderSyncFrontiers,
    /// Durable best header tip loaded from state.
    pub best_header_tip: Option<(block::Height, block::Hash)>,
    /// Hash of `frontiers.verified_block_tip`.
    pub verified_block_tip_hash: block::Hash,
}

impl ZakuraEndpoint {
    /// Returns the connector injected into the legacy handshake path.
    pub fn connector(&self) -> super::ZakuraHandshakeConnector {
        super::ZakuraHandshakeConnector::new_with_endpoint(self.clone())
    }

    /// Returns our local Zakura dial hints (iroh node id, and direct addresses
    /// each encoded as a `SocketAddr` string) for the legacy upgrade prelude.
    ///
    /// Direct addresses are capped at [`MAX_IROH_DIRECT_ADDRESSES`](super::MAX_IROH_DIRECT_ADDRESSES)
    /// so the encoded prelude stays within its bounded hint limits.
    pub(crate) async fn local_upgrade_hints(&self) -> (Vec<u8>, Vec<Vec<u8>>) {
        let endpoint = self.router.endpoint();
        let node_id = endpoint.node_id().as_bytes().to_vec();
        let node_addr = endpoint.node_addr().initialized().await;
        let direct_addresses = node_addr
            .direct_addresses()
            .take(super::MAX_IROH_DIRECT_ADDRESSES)
            .map(|addr| addr.to_string().into_bytes())
            .collect();
        (node_id, direct_addresses)
    }

    /// Returns the active supervisor handle.
    pub fn supervisor(&self) -> ZakuraSupervisorHandle {
        self.supervisor.clone()
    }

    /// Returns the endpoint trace emitter.
    pub fn trace(&self) -> ZakuraTrace {
        self.handler.trace.clone()
    }

    /// Returns the header-sync handle when native header sync is active.
    pub fn header_sync(&self) -> Option<super::HeaderSyncHandle> {
        self.header_sync.clone()
    }

    /// Returns the block-sync handle when native block sync is active.
    pub fn block_sync(&self) -> Option<BlockSyncHandle> {
        self.block_sync.clone()
    }

    /// Subscribe to the shared Zakura sync frontier stream.
    pub fn subscribe_sync_frontier(&self) -> Option<watch::Receiver<FrontierUpdate>> {
        self.sync_frontier
            .as_ref()
            .map(ZakuraSyncExchange::subscribe_frontier)
    }

    /// Return the currently cached shared Zakura sync frontier update.
    pub fn current_sync_frontier(&self) -> Option<FrontierUpdate> {
        self.sync_frontier
            .as_ref()
            .map(ZakuraSyncExchange::current_frontier)
    }

    /// Publish a shared Zakura sync frontier update.
    pub fn publish_sync_frontier(&self, update: FrontierUpdate) {
        self.publish_sync_frontier_from(update, "unknown");
    }

    /// Publish a shared Zakura sync frontier update with a trace source.
    pub fn publish_sync_frontier_from(&self, update: FrontierUpdate, source: &'static str) {
        if let Some(sync_frontier) = &self.sync_frontier {
            sync_frontier.publish_frontier(update, source);
        }
    }

    /// Take the header-sync action receiver when this endpoint was started in external-driver mode.
    pub async fn take_header_sync_actions(&self) -> Option<mpsc::Receiver<HeaderSyncAction>> {
        let actions = self.header_sync_actions.as_ref()?;
        actions.lock().await.take()
    }

    /// Take the block-sync action receiver when this endpoint was started in external-driver mode.
    pub async fn take_block_sync_actions(&self) -> Option<mpsc::Receiver<BlockSyncAction>> {
        let actions = self.block_sync_actions.as_ref()?;
        actions.lock().await.take()
    }

    /// Returns the endpoint-owned header-sync shutdown token.
    pub fn header_sync_shutdown(&self) -> Option<CancellationToken> {
        self.header_sync_tasks
            .as_ref()
            .map(|tasks| tasks.shutdown.clone())
    }

    /// The endpoint-wide background-task shutdown token, cancelled by
    /// [`ZakuraEndpoint::shutdown`].
    ///
    /// Detached dial/discovery loops observe this so they stop promptly at
    /// teardown instead of running on against a torn-down router. They each hold
    /// an endpoint clone, which keeps the supervisor registration watch open, so
    /// watch closure is *not* a reliable exit signal for them; this token is.
    /// Falls back to an un-cancelled token for endpoints built without a
    /// background-task owner (recorder-only test nodes).
    pub(crate) fn background_shutdown_token(&self) -> CancellationToken {
        self.header_sync_tasks
            .as_ref()
            .map(|tasks| tasks.shutdown.clone())
            .unwrap_or_default()
    }

    /// Track a header-sync integration task under the endpoint shutdown owner.
    pub async fn push_header_sync_task(&self, task: JoinHandle<()>) {
        if let Some(tasks) = self.header_sync_tasks.as_ref() {
            tasks.tasks.lock().await.push(task);
        }
    }

    /// Track a block-sync integration task under the endpoint shutdown owner.
    pub async fn push_block_sync_task(&self, task: JoinHandle<()>) {
        self.push_header_sync_task(task).await;
    }

    /// Returns the endpoint's current direct node address.
    pub async fn node_addr(&self) -> NodeAddr {
        self.router.endpoint().node_addr().initialized().await
    }

    /// Teach this endpoint how to reach a peer directly.
    pub fn add_node_addr(
        &self,
        node_addr: NodeAddr,
    ) -> Result<(), iroh::endpoint::AddNodeAddrError> {
        self.router.endpoint().add_node_addr(node_addr)
    }

    /// Start a native Zakura dial in the background, maintaining it with
    /// bounded backoff.
    ///
    /// Used by the legacy->Zakura upgrade hand-off: the legacy handshake just
    /// proved the peer is live, so a transient QUIC dial miss (e.g. the peer's
    /// endpoint is momentarily not ready) is worth retrying promptly instead of
    /// waiting for the legacy crawler to re-dial and re-run the whole upgrade.
    /// Once the legacy TCP connection is dropped, this task is the prompt
    /// recovery path for short Zakura disconnects; the address-book liveness
    /// keeper prevents the slower legacy crawler from churning while this peer
    /// remains registered.
    pub fn spawn_native_dial(&self, node_addr: NodeAddr) -> tokio::task::JoinHandle<()> {
        let endpoint = self.clone();
        let limits = self.handler.limits.clone();
        let policy = RedialPolicy::maintain(
            DEFAULT_ZAKURA_REDIAL_INITIAL_BACKOFF,
            DEFAULT_ZAKURA_REDIAL_MAX_BACKOFF,
        );
        tokio::spawn(native_dial_supervised(endpoint, node_addr, limits, policy))
    }

    /// Ensure there is one maintained native dial spawned by the legacy upgrade path.
    ///
    /// The legacy crawler can retry the same peer while a short-lived upgraded
    /// connection is still settling. Deduplicate those retries so repeated
    /// legacy upgrades do not create a swarm of independent maintained QUIC
    /// dial loops to the same peer.
    pub(crate) fn ensure_upgrade_native_dial(&self, node_addr: NodeAddr) -> bool {
        let Ok(peer_id) = ZakuraPeerId::new(node_addr.node_id.as_bytes().to_vec()) else {
            return false;
        };

        // Hold the registry lock across the spawn so the dedup check and the
        // abort-handle insert are atomic against a concurrent upgrade to the
        // same peer. `tokio::spawn` does not await, and the spawned task only
        // re-locks after its (non-instant) dial returns, so this cannot
        // deadlock or `.await` under the lock.
        let mut upgrade_dials = self
            .upgrade_dials
            .lock()
            .expect("Zakura upgrade dial registry mutex is never poisoned");
        if upgrade_dials.contains_key(&peer_id) {
            return true;
        }

        let endpoint = self.clone();
        let limits = self.handler.limits.clone();
        let policy = RedialPolicy::maintain(
            DEFAULT_ZAKURA_REDIAL_INITIAL_BACKOFF,
            DEFAULT_ZAKURA_REDIAL_MAX_BACKOFF,
        );
        let task_peer_id = peer_id.clone();
        let dial = tokio::spawn(async move {
            native_dial_supervised(endpoint.clone(), node_addr, limits, policy).await;
            endpoint
                .upgrade_dials
                .lock()
                .expect("Zakura upgrade dial registry mutex is never poisoned")
                .remove(&task_peer_id);
        });
        upgrade_dials.insert(peer_id, dial.abort_handle());
        true
    }

    /// Cancel and forget the maintained native dial started by the legacy
    /// upgrade hand-off for `peer_id`, if this node still owns one.
    ///
    /// Called when the upgrade hand-off wait times out without the peer
    /// registering: the maintained dial uses [`RedialPolicy::maintain`], so it
    /// would otherwise redial a peer-supplied, possibly unreachable address
    /// forever and keep its `upgrade_dials` entry. Repeating the failed upgrade
    /// with distinct node ids would then grow maintained dial tasks and
    /// outbound QUIC traffic without bound. A no-op if the peer already
    /// registered (its entry is reclaimed only when the maintained dial ends on
    /// shutdown) or if another upgrade owns the dedup slot.
    pub(crate) fn cancel_upgrade_native_dial(&self, peer_id: &ZakuraPeerId) {
        let handle = self
            .upgrade_dials
            .lock()
            .expect("Zakura upgrade dial registry mutex is never poisoned")
            .remove(peer_id);
        if let Some(handle) = handle {
            handle.abort();
        }
    }

    /// Returns whether the local admission semaphore has a free permit, i.e.
    /// whether this node can accept another inbound/dialed Zakura connection.
    /// Used by the discovery dialer to avoid starting candidate dials that would
    /// immediately bounce off the admission cap.
    pub(crate) fn has_native_admission_capacity(&self) -> bool {
        self.handler.admission.available_permits() > 0
    }

    /// Shut down the Router's ordered accept/handler lifecycle.
    pub async fn shutdown(&self) {
        if let Some(tasks) = &self.header_sync_tasks {
            tasks.shutdown.cancel();
            let mut tasks = tasks.tasks.lock().await;
            for mut task in tasks.drain(..) {
                if timeout(Duration::from_secs(1), &mut task).await.is_err() {
                    task.abort();
                    let _ = task.await;
                }
            }
        }
        self.supervisor.shutdown();
        let _ = self.router.shutdown().await;
    }

    #[cfg(any(test, feature = "zakura-testkit"))]
    pub(crate) fn from_parts(
        router: Router,
        supervisor: ZakuraSupervisorHandle,
        handler: ZakuraProtocolHandler,
    ) -> Self {
        Self {
            router,
            supervisor,
            handler,
            header_sync: None,
            block_sync: None,
            sync_frontier: None,
            header_sync_tasks: None,
            header_sync_actions: None,
            block_sync_actions: None,
            upgrade_dials: Arc::new(StdMutex::new(HashMap::new())),
        }
    }

    #[cfg(any(test, feature = "zakura-testkit"))]
    #[allow(dead_code)]
    pub(crate) fn from_parts_with_header_sync(
        router: Router,
        supervisor: ZakuraSupervisorHandle,
        handler: ZakuraProtocolHandler,
        header_sync: super::HeaderSyncHandle,
        shutdown: CancellationToken,
        tasks: Vec<JoinHandle<()>>,
        actions: Option<mpsc::Receiver<HeaderSyncAction>>,
    ) -> Self {
        Self {
            router,
            supervisor,
            handler,
            header_sync: Some(header_sync),
            block_sync: None,
            sync_frontier: None,
            header_sync_tasks: Some(Arc::new(HeaderSyncBackgroundTasks {
                shutdown,
                tasks: Mutex::new(tasks),
            })),
            header_sync_actions: actions.map(|actions| Arc::new(Mutex::new(Some(actions)))),
            block_sync_actions: None,
            upgrade_dials: Arc::new(StdMutex::new(HashMap::new())),
        }
    }

    #[cfg(any(test, feature = "zakura-testkit"))]
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn from_parts_with_sync_services(
        router: Router,
        supervisor: ZakuraSupervisorHandle,
        handler: ZakuraProtocolHandler,
        header_sync: super::HeaderSyncHandle,
        block_sync: BlockSyncHandle,
        shutdown: CancellationToken,
        tasks: Vec<JoinHandle<()>>,
        header_sync_actions: Option<mpsc::Receiver<HeaderSyncAction>>,
        block_sync_actions: Option<mpsc::Receiver<BlockSyncAction>>,
    ) -> Self {
        Self {
            router,
            supervisor,
            handler,
            header_sync: Some(header_sync),
            block_sync: Some(block_sync),
            sync_frontier: None,
            header_sync_tasks: Some(Arc::new(HeaderSyncBackgroundTasks {
                shutdown,
                tasks: Mutex::new(tasks),
            })),
            header_sync_actions: header_sync_actions
                .map(|actions| Arc::new(Mutex::new(Some(actions)))),
            block_sync_actions: block_sync_actions
                .map(|actions| Arc::new(Mutex::new(Some(actions)))),
            upgrade_dials: Arc::new(StdMutex::new(HashMap::new())),
        }
    }
}

/// Shared supervisor handle for Zakura peer registration and outbound work.
#[derive(Clone, Debug)]
pub struct ZakuraSupervisorHandle {
    id: u64,
    inner: Arc<Mutex<ZakuraSupervisorState>>,
    shutdown: CancellationToken,
    peer_set_tx: watch::Sender<Vec<ZakuraPeerId>>,
}

static NEXT_SUPERVISOR_ID: AtomicU64 = AtomicU64::new(1);

#[derive(Debug)]
struct ZakuraSupervisorState {
    supervisor: ZakuraPeerSupervisor,
    active_by_peer: HashMap<ZakuraPeerId, [u8; TRANSCRIPT_HASH_BYTES]>,
    outbound_by_peer: HashMap<ZakuraPeerId, ZakuraPeerHandle>,
    disconnect_by_peer: HashMap<ZakuraPeerId, CancellationToken>,
    caps_by_peer: HashMap<ZakuraPeerId, u64>,
    /// When each authenticated peer's current connection registered, used to
    /// decide whether a duplicate may evict a stale incumbent (see
    /// [`ZAKURA_DUPLICATE_EVICT_MIN_AGE`]).
    registered_at: HashMap<ZakuraPeerId, Instant>,
    active_by_ip: HashMap<IpAddr, usize>,
    max_connections_per_ip: usize,
}

/// Queue-backed outbound send handle for one authenticated Zakura peer.
#[derive(Clone, Debug)]
pub struct ZakuraPeerHandle {
    peer_id: ZakuraPeerId,
    sender: mpsc::Sender<ZakuraOutboundFrame>,
}

impl ZakuraPeerHandle {
    #[cfg(test)]
    pub(crate) fn new_for_tests(
        peer_id: ZakuraPeerId,
        sender: mpsc::Sender<ZakuraOutboundFrame>,
    ) -> Self {
        Self { peer_id, sender }
    }

    /// Authenticated peer id for this handle.
    pub fn peer_id(&self) -> &ZakuraPeerId {
        &self.peer_id
    }

    fn has_outbound_capacity(&self) -> bool {
        self.sender.capacity() > 0
    }

    /// Open a request stream, write one frame, then return the response frames from the same stream.
    pub async fn request(
        &self,
        stream_kind: u16,
        request_id: u64,
        message_type: u16,
        flags: u16,
        payload: Vec<u8>,
    ) -> Result<Vec<Frame>, BoxError> {
        let (completion, completed) = oneshot::channel();
        let frame = ZakuraOutboundFrame::Request {
            stream_kind,
            request_id,
            message_type,
            flags,
            payload,
            completion,
        };
        self.sender
            .send(frame)
            .await
            .map_err(|_| -> BoxError { "Zakura outbound peer queue closed".into() })?;
        completed
            .await
            .map_err(|_| -> BoxError { "Zakura outbound completion dropped".into() })?
    }
}

/// Request/response outbound work owned by a connection-serving task.
#[derive(Debug)]
pub enum ZakuraOutboundFrame {
    /// Compatibility request stream frame expecting response frames on the same stream.
    Request {
        /// Application stream kind to open.
        stream_kind: u16,
        /// Request id written into the stream prelude.
        request_id: u64,
        /// Application message type.
        message_type: u16,
        /// Message flags.
        flags: u16,
        /// Message payload bytes.
        payload: Vec<u8>,
        /// Completion sent with decoded raw response frames or an error.
        completion: oneshot::Sender<Result<Vec<Frame>, BoxError>>,
    },
}

impl ZakuraSupervisorHandle {
    /// Create an empty supervisor.
    pub fn new(max_connections_per_ip: usize) -> Self {
        Self {
            id: NEXT_SUPERVISOR_ID.fetch_add(1, Ordering::Relaxed),
            inner: Arc::new(Mutex::new(ZakuraSupervisorState {
                supervisor: ZakuraPeerSupervisor::default(),
                active_by_peer: HashMap::new(),
                outbound_by_peer: HashMap::new(),
                disconnect_by_peer: HashMap::new(),
                caps_by_peer: HashMap::new(),
                registered_at: HashMap::new(),
                active_by_ip: HashMap::new(),
                max_connections_per_ip: max_connections_per_ip.max(1),
            })),
            shutdown: CancellationToken::new(),
            peer_set_tx: watch::channel(Vec::new()).0,
        }
    }

    /// Returns the currently registered authenticated Zakura peer ids.
    pub async fn registered_ids(&self) -> Vec<ZakuraPeerId> {
        let state = self.inner.lock().await;
        state.active_by_peer.keys().cloned().collect()
    }

    pub(crate) fn id(&self) -> u64 {
        self.id
    }

    /// Returns queue-backed handles for peers currently able to accept outbound work.
    pub async fn outbound_peer_handles(&self) -> Vec<ZakuraPeerHandle> {
        let state = self.inner.lock().await;
        state
            .outbound_by_peer
            .values()
            .filter(|handle| handle.has_outbound_capacity())
            .cloned()
            .collect()
    }

    /// Subscribe to peer-set changes for event-driven tests and diagnostics.
    pub fn subscribe(&self) -> watch::Receiver<Vec<ZakuraPeerId>> {
        self.peer_set_tx.subscribe()
    }

    /// Disconnect one active Zakura peer.
    pub async fn disconnect_peer(&self, peer_id: &ZakuraPeerId) -> bool {
        let token = {
            let state = self.inner.lock().await;
            state.disconnect_by_peer.get(peer_id).cloned()
        };

        if let Some(token) = token {
            token.cancel();
            true
        } else {
            false
        }
    }

    async fn register(
        &self,
        peer_id: ZakuraPeerId,
        remote_ip: Option<IpAddr>,
        transcript_hash: [u8; TRANSCRIPT_HASH_BYTES],
        outbound_handle: ZakuraPeerHandle,
        disconnect_token: CancellationToken,
        accepted_capabilities: u64,
    ) -> ZakuraRegistration {
        let mut state = self.inner.lock().await;
        // A re-registration for a peer id that is already active is a duplicate
        // redial, not a new connection: the incumbent already holds the per-IP
        // slot, and the duplicate branch below either keeps the incumbent and
        // closes the newcomer or evicts a stale incumbent in its place, so it
        // never consumes an additional per-IP slot. Exempting duplicates from the
        // per-IP cap precheck lets a same-peer redial from an IP already at the cap
        // reach the stale-incumbent eviction path instead of being rejected as a
        // resource limit, so a dead incumbent is evicted in milliseconds rather
        // than blocking the peer until the QUIC idle timeout (~150s).
        let is_duplicate_redial = state.active_by_peer.contains_key(&peer_id);
        if let Some(remote_ip) = remote_ip {
            if !is_duplicate_redial {
                let ip_count = state
                    .active_by_ip
                    .get(&remote_ip)
                    .copied()
                    .unwrap_or_default();
                if ip_count >= state.max_connections_per_ip {
                    metrics::counter!("zakura.p2p.conn.rejected.admission").increment(1);
                    return ZakuraRegistration::Rejected(ZakuraRejectReason::ResourceLimit);
                }
            }
        }

        match state
            .supervisor
            .register_authenticated(peer_id.clone(), transcript_hash)
        {
            ZakuraUpgradeOutcome::Upgraded { .. } => {
                if let Some(remote_ip) = remote_ip {
                    *state.active_by_ip.entry(remote_ip).or_default() += 1;
                }
                state
                    .active_by_peer
                    .insert(peer_id.clone(), transcript_hash);
                state
                    .outbound_by_peer
                    .insert(peer_id.clone(), outbound_handle);
                state
                    .disconnect_by_peer
                    .insert(peer_id.clone(), disconnect_token);
                state
                    .caps_by_peer
                    .insert(peer_id.clone(), accepted_capabilities);
                state.registered_at.insert(peer_id.clone(), Instant::now());
                let registered_ids: Vec<_> = state.active_by_peer.keys().cloned().collect();
                set_active_connection_gauge(registered_ids.len());
                self.peer_set_tx.send_replace(registered_ids);
                let disconnect_token = state
                    .disconnect_by_peer
                    .get(&peer_id)
                    .cloned()
                    .expect("disconnect token exists because this peer was just registered");
                ZakuraRegistration::Registered {
                    peer_id,
                    remote_ip,
                    disconnect_token,
                }
            }
            ZakuraUpgradeOutcome::Duplicate { .. } => {
                // A duplicate for an identity that already has a connection is
                // almost always a restart or redial. If the incumbent has been
                // registered long enough to be a stale connection left behind by
                // a restarted peer, cancel it now so it tears down through its
                // normal cleanup path and frees the slot in milliseconds; the
                // peer's redial then takes the freed slot instead of waiting for
                // the dead connection's QUIC idle timeout (~150s). A young
                // incumbent is kept to avoid flapping on simultaneous-open races.
                // The newcomer is still closed (its redial reconnects cleanly
                // once the slot is free), which avoids racing the incumbent's
                // service-registration teardown.
                if let Some(registered_at) = state.registered_at.get(&peer_id) {
                    if registered_at.elapsed() >= ZAKURA_DUPLICATE_EVICT_MIN_AGE {
                        if let Some(token) = state.disconnect_by_peer.get(&peer_id) {
                            token.cancel();
                            metrics::counter!("zakura.p2p.conn.duplicate.evicted_stale")
                                .increment(1);
                        }
                    }
                }
                ZakuraRegistration::Duplicate { peer_id }
            }
            ZakuraUpgradeOutcome::Rejected { reason } => ZakuraRegistration::Rejected(reason),
        }
    }

    async fn deregister(&self, peer_id: &ZakuraPeerId, remote_ip: Option<IpAddr>) {
        let mut state = self.inner.lock().await;
        state.active_by_peer.remove(peer_id);
        state.outbound_by_peer.remove(peer_id);
        state.disconnect_by_peer.remove(peer_id);
        state.caps_by_peer.remove(peer_id);
        state.registered_at.remove(peer_id);
        if let Some(remote_ip) = remote_ip {
            if let Some(count) = state.active_by_ip.get_mut(&remote_ip) {
                *count = count.saturating_sub(1);
                if *count == 0 {
                    state.active_by_ip.remove(&remote_ip);
                }
            }
        }
        state.supervisor.deregister_authenticated(peer_id);
        let registered_ids: Vec<_> = state.active_by_peer.keys().cloned().collect();
        set_active_connection_gauge(registered_ids.len());
        self.peer_set_tx.send_replace(registered_ids);
    }

    fn shutdown(&self) {
        self.shutdown.cancel();
    }

    /// Returns whether another connection from `remote_ip` would stay within the
    /// per-IP cap, counting `in_flight_count` dials this caller already has in
    /// flight to that IP. Used by the discovery dialer to reserve per-IP slots
    /// before launching a candidate dial.
    pub(crate) async fn can_accept_remote_ip_with_in_flight(
        &self,
        remote_ip: IpAddr,
        in_flight_count: usize,
    ) -> bool {
        let state = self.inner.lock().await;
        let active_count = state
            .active_by_ip
            .get(&remote_ip)
            .copied()
            .unwrap_or_default();
        active_count.saturating_add(in_flight_count) < state.max_connections_per_ip
    }
}

fn set_active_connection_gauge(active_connections: usize) {
    // Active Zakura connections are bounded by the configured connection limit,
    // far below f64's exact integer range.
    metrics::gauge!("zakura.p2p.conn.active").set(active_connections as f64);
}

#[derive(Debug)]
enum ZakuraRegistration {
    Registered {
        peer_id: ZakuraPeerId,
        remote_ip: Option<IpAddr>,
        disconnect_token: CancellationToken,
    },
    Duplicate {
        peer_id: ZakuraPeerId,
    },
    Rejected(ZakuraRejectReason),
}

#[derive(Clone, Debug)]
struct ZakuraConnTrace {
    id: u64,
    peer_label: Option<Arc<str>>,
}

impl ZakuraConnTrace {
    fn new(trace: &ZakuraTrace, id: u64, peer_id: &ZakuraPeerId) -> Self {
        Self {
            id,
            peer_label: trace
                .is_enabled()
                .then(|| Arc::<str>::from(trace_peer_label(peer_id))),
        }
    }

    fn without_peer(id: u64) -> Self {
        Self {
            id,
            peer_label: None,
        }
    }

    #[cfg(any(test, feature = "zakura-testkit"))]
    fn placeholder() -> Self {
        Self::without_peer(0)
    }

    fn peer(&self) -> Option<&str> {
        self.peer_label.as_deref()
    }

    fn event<'a>(&'a self, event: &'static str) -> ZakuraTraceEvent<'a> {
        ZakuraTraceEvent::new(event)
            .conn(self.id)
            .maybe_peer(self.peer())
    }
}

struct StreamAdmission<'a> {
    trace: ZakuraTrace,
    conn: ZakuraConnTrace,
    peer_id: &'a ZakuraPeerId,
    stream_sem: &'a Arc<Semaphore>,
    open_limiter: &'a mut TokenBucket,
    message_buckets: &'a mut MessageRateBuckets,
    workers: &'a mut JoinSet<()>,
    limits: ZakuraConnectionLimits,
    accepted_capabilities: u64,
    connection_token: CancellationToken,
    freshness_tx: watch::Sender<Instant>,
}

impl StreamAdmission<'_> {
    fn event(&self, event: &'static str, stream_id: u64) -> ZakuraTraceEvent<'_> {
        self.conn.event(event).stream(stream_id)
    }
}

struct ConnectionServeContext {
    limits: ZakuraConnectionLimits,
    accepted_capabilities: u64,
    role: &'static str,
    direction: ServicePeerDirection,
    transcript_hash: [u8; TRANSCRIPT_HASH_BYTES],
    conn: ZakuraConnTrace,
}

struct RegisteredConnectionServeContext {
    limits: ZakuraConnectionLimits,
    conn: ZakuraConnTrace,
    connection_token: CancellationToken,
    accepted_capabilities: u64,
    opens_ordered_streams: bool,
    direction: ServicePeerDirection,
}

struct StreamWorkerContext {
    trace: ZakuraTrace,
    conn: ZakuraConnTrace,
    peer_id: ZakuraPeerId,
    stream_id: u64,
    _permit: OwnedSemaphorePermit,
    limits: ZakuraConnectionLimits,
    message_bucket: SharedMessageBucket,
    connection_token: CancellationToken,
    stream_token: CancellationToken,
    freshness_tx: watch::Sender<Instant>,
}

impl StreamWorkerContext {
    fn event(&self, event: &'static str) -> ZakuraTraceEvent<'_> {
        self.conn.event(event).stream(self.stream_id)
    }
}

struct AdmittedOrderedStream {
    kind: u16,
    recv: FramedRecv,
    send: FramedSend,
    cancel_token: CancellationToken,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum InboundMessageAdmission {
    Admit,
    Oversize,
    Throttled,
}

fn admit_inbound_message(
    payload_len: usize,
    context: &StreamWorkerContext,
    stream_kind: u16,
) -> InboundMessageAdmission {
    let stream_kind = stream_kind_label(stream_kind);
    let max_message_bytes = usize::try_from(context.limits.max_message_bytes)
        .expect("u32 message byte limit fits in usize");
    if payload_len > max_message_bytes {
        metrics::counter!(
            "zakura.p2p.ratelimit.message.oversize",
            "stream_kind" => stream_kind,
        )
        .increment(1);
        context.trace.emit(
            RATELIMIT_TABLE,
            context.event("message.oversize").stream_kind(stream_kind),
        );
        return InboundMessageAdmission::Oversize;
    }

    let admitted = {
        let mut bucket = context
            .message_bucket
            .lock()
            .expect("Zakura message-rate bucket mutex is never poisoned");
        bucket.try_take()
    };
    if !admitted {
        metrics::counter!(
            "zakura.p2p.ratelimit.message.throttled",
            "stream_kind" => stream_kind,
        )
        .increment(1);
        context.trace.emit(
            RATELIMIT_TABLE,
            context.event("message.throttled").stream_kind(stream_kind),
        );
        return InboundMessageAdmission::Throttled;
    }

    InboundMessageAdmission::Admit
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) struct NativeHandshakeNegotiated {
    pub(crate) limits: ZakuraAcceptedLimits,
    pub(crate) accepted_capabilities: u64,
}

pub(crate) fn service_registry(
    _supervisor: &ZakuraSupervisorHandle,
    header_sync: Option<super::HeaderSyncHandle>,
    block_sync: Option<BlockSyncHandle>,
    block_sync_config: ZakuraBlockSyncConfig,
    legacy_service: Arc<dyn Service>,
    discovery_service: Arc<dyn Service>,
) -> Result<Arc<ServiceRegistry>, BoxError> {
    let mut services = vec![legacy_service.clone(), discovery_service];
    if let Some(header_sync) = &header_sync {
        services.push(Arc::new(HeaderSyncService::new(header_sync.clone())) as Arc<dyn Service>);
    } else {
        services
            .push(Arc::new(HeaderSyncPassthroughService::new(legacy_service)) as Arc<dyn Service>);
    }
    let block_sync = match block_sync {
        Some(block_sync) => BlockSyncService::new_with_handle(block_sync_config, block_sync),
        None => match header_sync.as_ref() {
            Some(header_sync) => BlockSyncService::new_with_header_tip(
                block_sync_config,
                header_sync.subscribe_tip(),
            ),
            None => BlockSyncService::new(block_sync_config),
        },
    };
    services.push(Arc::new(block_sync) as Arc<dyn Service>);

    Ok(Arc::new(
        ServiceRegistry::new(services).map_err(|error| -> BoxError { Box::new(error) })?,
    ))
}

/// Iroh protocol handler for the Zakura `p2p-v2/1` ALPN.
#[derive(Debug, Clone)]
pub struct ZakuraProtocolHandler {
    supervisor: ZakuraSupervisorHandle,
    handshake_config: ZakuraHandshakeConfig,
    limits: ZakuraLocalLimits,
    registry: Arc<ServiceRegistry>,
    trace: ZakuraTrace,
    next_conn_id: Arc<AtomicU64>,
    next_stream_id: Arc<AtomicU64>,
    admission: Arc<Semaphore>,
    pending_handshakes: Arc<Semaphore>,
    shutdown: CancellationToken,
    // Bound iroh endpoint, used to recover the inbound peer's UDP source IP so
    // the per-IP admission cap applies to Router-accepted connections. Iroh's
    // Router consumes the `Incoming` (which carries the address) before
    // `ProtocolHandler::accept` hands us the established `Connection`, so the
    // address is looked up from the endpoint's node map instead. `None` for
    // unit tests that drive the handler without a bound endpoint, in which case
    // inbound accepts fall back to the previous `remote_ip = None` behaviour.
    endpoint: Option<Endpoint>,
}

/// Resolve the inbound peer's UDP source IP from the endpoint's node map so the
/// per-IP connection cap can be enforced for Router-accepted connections.
///
/// Iroh exposes the peer address on `Incoming`, which the Router consumes before
/// `ProtocolHandler::accept`. After the native control handshake the connection
/// has exchanged real QUIC payload over its direct path, so the endpoint's node
/// map knows the peer's UDP address: prefer the connection's confirmed current
/// path (`conn_type`), then fall back to the direct address that most recently
/// delivered payload from this peer. Relay-only paths have no single
/// attributable source IP, so they correctly yield `None` (the global
/// connection cap still bounds them).
fn inbound_remote_ip(endpoint: &Endpoint, node_id: NodeId) -> Option<IpAddr> {
    if let Some(mut conn_type) = endpoint.conn_type(node_id) {
        match conn_type.get() {
            ConnectionType::Direct(addr) | ConnectionType::Mixed(addr, _) => {
                return Some(addr.ip())
            }
            ConnectionType::Relay(_) | ConnectionType::None => {}
        }
    }
    endpoint.remote_info(node_id).and_then(|info| {
        info.addrs
            .into_iter()
            .filter(|addr| addr.last_payload.is_some())
            // Smallest elapsed-since-last-payload is the path actively carrying
            // this connection's traffic, i.e. the real inbound source address.
            .min_by_key(|addr| addr.last_payload)
            .map(|addr| addr.addr.ip())
    })
}

fn native_connection_transcript_hash(
    direction: ServicePeerDirection,
    local_node_id: &NodeId,
    remote_node_id: &NodeId,
) -> [u8; TRANSCRIPT_HASH_BYTES] {
    let initiator = match direction {
        ServicePeerDirection::Inbound => remote_node_id,
        ServicePeerDirection::Outbound => local_node_id,
    };
    *initiator.as_bytes()
}

impl ZakuraProtocolHandler {
    /// Create a handler sharing the given supervisor.
    pub fn new(
        supervisor: ZakuraSupervisorHandle,
        network: Network,
        handshake_config: ZakuraHandshakeConfig,
        limits: ZakuraLocalLimits,
    ) -> Self {
        Self::new_with_registry(
            supervisor,
            network,
            handshake_config,
            limits,
            Arc::new(ServiceRegistry::default()),
        )
    }

    /// Create a handler with an injected service registry.
    pub fn new_with_registry(
        supervisor: ZakuraSupervisorHandle,
        network: Network,
        handshake_config: ZakuraHandshakeConfig,
        limits: ZakuraLocalLimits,
        registry: Arc<ServiceRegistry>,
    ) -> Self {
        Self::new_with_registry_and_trace(
            supervisor,
            network,
            handshake_config,
            limits,
            registry,
            ZakuraTrace::noop(),
        )
    }

    /// Create a handler with an injected service registry and trace emitter.
    pub fn new_with_registry_and_trace(
        supervisor: ZakuraSupervisorHandle,
        _network: Network,
        handshake_config: ZakuraHandshakeConfig,
        limits: ZakuraLocalLimits,
        registry: Arc<ServiceRegistry>,
        trace: ZakuraTrace,
    ) -> Self {
        let mut handshake_config = handshake_config;
        handshake_config.supported_capabilities = registry.supported_capabilities();
        Self {
            supervisor,
            handshake_config,
            registry,
            trace,
            next_conn_id: Arc::new(AtomicU64::new(1)),
            next_stream_id: Arc::new(AtomicU64::new(1)),
            admission: Arc::new(Semaphore::new(limits.max_connections)),
            pending_handshakes: Arc::new(Semaphore::new(limits.max_pending_handshakes)),
            shutdown: CancellationToken::new(),
            limits,
            endpoint: None,
        }
    }

    /// Attach the bound iroh endpoint so inbound Router-accepted connections can
    /// resolve the peer's UDP source IP and enforce the per-IP connection cap.
    pub fn with_endpoint(mut self, endpoint: Endpoint) -> Self {
        self.endpoint = Some(endpoint);
        self
    }

    async fn accept_connection(&self, connection: Connection) -> Result<(), AcceptError> {
        let conn_id = self.next_conn_id.fetch_add(1, Ordering::Relaxed);
        let Ok(_admission) = self.admission.clone().try_acquire_owned() else {
            metrics::counter!("zakura.p2p.conn.rejected.admission").increment(1);
            let conn = ZakuraConnTrace::without_peer(conn_id);
            self.trace.emit(
                CONN_TABLE,
                conn.event("rejected.admission")
                    .direction("inbound")
                    .reason("admission"),
            );
            connection.close(VarInt::from_u32(ZAKURA_CLOSE_RESOURCE), b"admission");
            return Ok(());
        };

        let remote_node_id = connection.remote_node_id()?;
        let remote_peer_id =
            ZakuraPeerId::new(remote_node_id.as_bytes().to_vec()).map_err(AcceptError::from_err)?;
        let conn = ZakuraConnTrace::new(&self.trace, conn_id, &remote_peer_id);

        let negotiated = match self
            .run_native_responder_handshake_with_permit(&connection, &remote_peer_id, &conn)
            .await
        {
            Ok(negotiated) => negotiated,
            Err(ZakuraHandlerError::ResourceLimit("pending handshake")) => return Ok(()),
            Err(error) => {
                debug!(?error, "Zakura control handshake failed");
                connection.close(VarInt::from_u32(ZAKURA_CLOSE_NEUTRAL), b"control handshake");
                return Ok(());
            }
        };

        let conn_limits = self.limits.clamp(&negotiated.limits);
        // Iroh's Router hands ProtocolHandler only the established Connection;
        // the peer UDP address lives on Incoming, which the Router consumes
        // before this point. Recover it from the endpoint's node map so the
        // per-IP admission cap applies to inbound accepts and a single source
        // IP cannot fill the global connection budget with distinct node ids.
        // The handshake above has already exchanged QUIC payload over the
        // direct path, so the node map knows the peer's address. Native
        // outbound dials still pass the configured direct IP.
        let remote_ip = self
            .endpoint
            .as_ref()
            .and_then(|endpoint| inbound_remote_ip(endpoint, remote_node_id));
        let local_node_id = self
            .endpoint
            .as_ref()
            .map(|endpoint| endpoint.node_id())
            .unwrap_or(remote_node_id);
        let direction = ServicePeerDirection::Inbound;
        self.register_and_serve(
            connection,
            remote_peer_id,
            remote_ip,
            ConnectionServeContext {
                limits: conn_limits,
                accepted_capabilities: negotiated.accepted_capabilities,
                role: "responder",
                direction,
                transcript_hash: native_connection_transcript_hash(
                    direction,
                    &local_node_id,
                    &remote_node_id,
                ),
                conn,
            },
        )
        .await
        .map_err(AcceptError::from_err)
    }

    async fn run_native_responder_handshake_with_permit(
        &self,
        connection: &Connection,
        remote_peer_id: &ZakuraPeerId,
        conn: &ZakuraConnTrace,
    ) -> Result<NativeHandshakeNegotiated, ZakuraHandlerError> {
        let Ok(_handshake) = self.pending_handshakes.clone().try_acquire_owned() else {
            metrics::counter!("zakura.p2p.conn.rejected.pending_handshake").increment(1);
            self.trace.emit(
                CONN_TABLE,
                conn.event("rejected.admission")
                    .direction("inbound")
                    .reason("pending_handshake"),
            );
            connection.close(
                VarInt::from_u32(ZAKURA_CLOSE_RESOURCE),
                b"pending handshake",
            );
            return Err(ZakuraHandlerError::ResourceLimit("pending handshake"));
        };

        self.run_native_responder_handshake(connection, remote_peer_id, conn)
            .await
    }

    async fn run_native_responder_handshake(
        &self,
        connection: &Connection,
        remote_peer_id: &ZakuraPeerId,
        conn: &ZakuraConnTrace,
    ) -> Result<NativeHandshakeNegotiated, ZakuraHandlerError> {
        self.trace.emit(
            HANDSHAKE_TABLE,
            conn.event("control.started")
                .role("responder")
                .phase("control")
                .network(self.handshake_config.network_label()),
        );
        let (mut send, mut recv) = timeout(self.limits.control_timeout, connection.accept_bi())
            .await
            .map_err(|_| ZakuraHandlerError::Timeout("accept control stream"))??;

        let hello_bytes = read_control_payload(
            &mut recv,
            self.handshake_config.max_control_frame_bytes,
            self.limits.control_timeout,
        )
        .await?;
        let hello = ZakuraControlHello::decode(&hello_bytes)?;
        let expected = ZakuraControlValidation {
            local: &self.handshake_config,
            authenticated_remote_id: remote_peer_id.as_bytes(),
            selected_zakura_protocol: ZAKURA_PROTOCOL_VERSION_1,
            handshake_path: ZakuraHandshakePath::Native,
            remote_role: ZakuraControlRole::Initiator,
            initiator_upgrade_nonce: [0; 32],
            responder_upgrade_nonce: [0; 32],
            legacy_upgrade_transcript: [0; 32],
        };
        hello.validate(&expected)?;

        let mut local_nonce = [0; 32];
        OsRng.fill_bytes(&mut local_nonce);

        let accepted_limits = self.accepted_limits_for(&hello.initial_limits);
        let ack = ZakuraControlAck {
            magic: CONTROL_ACK_MAGIC,
            control_version: CONTROL_VERSION,
            selected_zakura_protocol: hello.selected_zakura_protocol,
            peer_nonce: local_nonce,
            remote_peer_nonce: hello.peer_nonce,
            accepted_capabilities: hello.capabilities
                & self.handshake_config.supported_capabilities,
            accepted_channels: hello.required_channels & self.handshake_config.supported_channels,
            accepted_limits,
        };
        write_control_payload(&mut send, &ack.encode()?, self.limits.control_timeout).await?;
        self.trace.emit(
            HANDSHAKE_TABLE,
            conn.event("control.succeeded")
                .role("responder")
                .phase("control")
                .selected_protocol(ack.selected_zakura_protocol)
                .network(self.handshake_config.network_label()),
        );
        Ok(NativeHandshakeNegotiated {
            limits: accepted_limits,
            accepted_capabilities: ack.accepted_capabilities,
        })
    }

    fn accepted_limits_for(&self, remote_limits: &ZakuraInitialLimits) -> ZakuraAcceptedLimits {
        ZakuraLimits {
            max_frame_bytes: remote_limits
                .max_frame_bytes
                .min(self.limits.max_frame_bytes),
            max_message_bytes: remote_limits
                .max_message_bytes
                .min(self.limits.max_message_bytes),
            max_open_streams: remote_limits
                .max_open_streams
                .min(self.limits.max_open_streams),
            max_inbound_queue_depth: remote_limits
                .max_inbound_queue_depth
                .min(self.limits.max_inbound_queue_depth),
            idle_timeout_millis: remote_limits
                .idle_timeout_millis
                .min(self.limits.initial_limits().idle_timeout_millis),
        }
    }

    async fn serve_connection(
        &self,
        connection: Connection,
        peer_id: ZakuraPeerId,
        remote_ip: Option<IpAddr>,
        mut outbound_rx: mpsc::Receiver<ZakuraOutboundFrame>,
        context: RegisteredConnectionServeContext,
    ) -> Result<(), ZakuraHandlerError> {
        let limits = context.limits;
        let conn = context.conn;
        let connection_token = context.connection_token;
        let accepted_capabilities = context.accepted_capabilities;
        let stream_sem = Arc::new(Semaphore::new(usize::from(limits.max_open_streams)));
        let mut workers = JoinSet::new();
        let mut open_limiter = TokenBucket::new(limits.stream_open_rate_per_second);
        let mut message_buckets = MessageRateBuckets::new();
        let (freshness_tx, freshness_rx) = watch::channel(Instant::now());
        // Bounded label describing why this connection closed. Updated at each
        // local teardown site and attached to the final `closed.neutral` trace
        // so readers can distinguish idle timeouts, peer-side closes, resource
        // limits, and protocol faults. The default covers the external path:
        // the connection token was cancelled by a disconnect request, peerset
        // eviction, or process shutdown.
        let mut close_reason: &'static str = "cancelled";
        let negotiated_ordered_streams = self
            .registry
            .ordered_streams_for_negotiated(accepted_capabilities);
        let ordered_streams = if context.opens_ordered_streams {
            self.registry.ordered_streams_for_escalation(
                accepted_capabilities,
                &peer_id,
                context.direction,
            )
        } else {
            Vec::new()
        };
        let request_response_stream_count = self
            .registry
            .request_response_streams_for_negotiated(accepted_capabilities)
            .len();
        if ordered_streams.len() > usize::from(limits.max_open_streams) {
            debug!(
                max_open_streams = limits.max_open_streams,
                ordered_stream_count = ordered_streams.len(),
                "closing Zakura peer because negotiated ordered streams exceed max-open-streams"
            );
            connection.close(VarInt::from_u32(ZAKURA_CLOSE_RESOURCE), b"ordered streams");
            close_reason = "resource_ordered_streams";
            connection_token.cancel();
        } else if !ordered_streams.is_empty()
            && usize::from(limits.max_inbound_queue_depth) < ordered_streams.len()
        {
            debug!(
                max_inbound_queue_depth = limits.max_inbound_queue_depth,
                ordered_stream_count = ordered_streams.len(),
                "closing Zakura peer because inbound queue depth cannot be split across ordered streams"
            );
            connection.close(VarInt::from_u32(ZAKURA_CLOSE_RESOURCE), b"queue split");
            close_reason = "resource_queue_split";
            connection_token.cancel();
        }
        let ordered_kinds: HashSet<u16> = negotiated_ordered_streams
            .iter()
            .map(|stream| stream.kind)
            .collect();
        let queue_split_stream_count = negotiated_ordered_streams.len().max(ordered_streams.len());
        let per_stream_queue_depth = per_stream_inbound_queue_depth(
            limits.max_inbound_queue_depth,
            queue_split_stream_count,
        );
        let mut service_streams = HashMap::new();
        let mut accepted_ordered_kinds = HashSet::new();
        let mut admitted_capabilities = 0;
        let run_freshness_reaper =
            should_run_freshness_reaper(queue_split_stream_count, request_response_stream_count);

        if negotiated_ordered_streams.is_empty() {
            self.registry.add_peer(Peer::new_with_direction(
                peer_id.clone(),
                remote_ip,
                accepted_capabilities,
                context.direction,
                HashMap::new(),
                connection_token.clone(),
            ));
            admitted_capabilities = accepted_capabilities;
        } else if context.opens_ordered_streams && !connection_token.is_cancelled() {
            let mut opened_capabilities = 0;
            for stream in ordered_streams {
                opened_capabilities |= stream.capability;
                let admitted = match self
                    .open_ordered_service_stream(
                        &connection,
                        stream,
                        &mut workers,
                        &stream_sem,
                        &mut message_buckets,
                        limits,
                        per_stream_queue_depth,
                        connection_token.clone(),
                        freshness_tx.clone(),
                        conn.clone(),
                        peer_id.clone(),
                    )
                    .await
                {
                    Ok(admitted) => admitted,
                    Err(error) => {
                        debug!(
                            ?error,
                            stream_kind = stream.kind,
                            "closing Zakura peer after ordered stream setup failed"
                        );
                        connection.close(
                            VarInt::from_u32(ZAKURA_CLOSE_RESOURCE),
                            b"ordered stream setup",
                        );
                        close_reason = "stream_setup_failed";
                        connection_token.cancel();
                        break;
                    }
                };
                service_streams.insert(
                    admitted.kind,
                    ServiceStream::new(admitted.recv, admitted.send, admitted.cancel_token),
                );
            }
            if !connection_token.is_cancelled() {
                // The initiator has already narrowed escalation to opened
                // ordered services. Disconnect fanout still uses the registry's
                // returned admitted mask, not this peer context.
                admitted_capabilities |=
                    self.registry
                        .add_escalated_peer(Peer::new_with_service_streams(
                            peer_id.clone(),
                            remote_ip,
                            opened_capabilities,
                            context.direction,
                            std::mem::take(&mut service_streams),
                            connection_token.clone(),
                        ));
            }
        }

        loop {
            tokio::select! {
                biased;
                _ = connection_token.cancelled() => break,
                _ = freshness_reaper(freshness_rx.clone(), limits.idle_timeout), if run_freshness_reaper => {
                    connection.close(VarInt::from_u32(ZAKURA_CLOSE_NEUTRAL), b"idle");
                    close_reason = "idle_timeout";
                    break;
                }
                Some(joined) = workers.join_next() => {
                    if let Err(error) = joined {
                        debug!(?error, "Zakura stream worker exited unexpectedly");
                    }
                }
                accepted = connection.accept_bi() => {
                    match accepted {
                        Ok((send, recv)) => {
                            let mut admission = StreamAdmission {
                                trace: self.trace.clone(),
                                conn: conn.clone(),
                                peer_id: &peer_id,
                                stream_sem: &stream_sem,
                                open_limiter: &mut open_limiter,
                                message_buckets: &mut message_buckets,
                                workers: &mut workers,
                                limits,
                                accepted_capabilities,
                                connection_token: connection_token.clone(),
                                freshness_tx: freshness_tx.clone(),
                            };
                            if let Some(admitted) = self
                                .admit_bi_stream(send, recv, &mut admission, per_stream_queue_depth)
                                .await
                            {
                                if context.opens_ordered_streams
                                    || !ordered_kinds.contains(&admitted.kind)
                                    || !accepted_ordered_kinds.insert(admitted.kind)
                                {
                                    debug!(
                                        stream_kind = admitted.kind,
                                        "closing peer after duplicate or unexpected ordered stream"
                                    );
                                    close_reason = "duplicate_stream";
                                    connection_token.cancel();
                                    continue;
                                }

                                if !self.registry.wants_ordered_stream(
                                    admitted.kind,
                                    accepted_capabilities,
                                    &peer_id,
                                    context.direction,
                                ) {
                                    debug!(
                                        stream_kind = admitted.kind,
                                        "locally parking ordered service stream because the service has no demand"
                                    );
                                    admitted.cancel_token.cancel();
                                    continue;
                                }

                                let service_streams = HashMap::from([(
                                    admitted.kind,
                                    ServiceStream::new(
                                        admitted.recv,
                                        admitted.send,
                                        admitted.cancel_token.clone(),
                                    ),
                                )]);
                                // Current ordered services own one ordered stream
                                // each, so the responder can fan out accepted
                                // streams one at a time. Batch here if a service
                                // gains multiple ordered streams.
                                //
                                // The responder keeps the full accepted capability
                                // context so discovery can make cross-service
                                // ownership decisions; disconnect fanout still
                                // uses the registry's returned admitted mask.
                                admitted_capabilities |= self.registry.add_escalated_peer(
                                    Peer::new_with_service_streams(
                                        peer_id.clone(),
                                        remote_ip,
                                        accepted_capabilities,
                                        context.direction,
                                        service_streams,
                                        connection_token.clone(),
                                    ),
                                );
                            }
                        }
                        Err(error) => {
                            debug!(?error, "Zakura connection stopped accepting streams");
                            close_reason = "accept_failed";
                            break;
                        }
                    }
                }
                outbound = outbound_rx.recv() => {
                    let Some(outbound) = outbound else {
                        close_reason = "outbound_closed";
                        break;
                    };
                    match outbound {
                        ZakuraOutboundFrame::Request {
                            stream_kind,
                            request_id,
                            message_type,
                            flags,
                            payload,
                            mut completion,
                        } => {
                            let result = tokio::select! {
                                biased;
                                _ = completion.closed() => {
                                    Err(OutboundRequestError::Local(
                                        "Zakura outbound request receiver dropped".into(),
                                    ))
                                }
                                result = write_outbound_request_frame(
                                    &connection,
                                    limits,
                                    stream_kind,
                                    request_id,
                                    message_type,
                                    flags,
                                    payload,
                                ) => result,
                            };
                            match result {
                                Ok(frames) => {
                                    let _ = completion.send(Ok(frames));
                                }
                                Err(OutboundRequestError::Local(error)) => {
                                    let _ = completion.send(Err(error));
                                }
                                Err(OutboundRequestError::Fatal(error)) => {
                                    connection.close(
                                        VarInt::from_u32(ZAKURA_CLOSE_BAD_PRELUDE),
                                        b"malformed response",
                                    );
                                    close_reason = "bad_response";
                                    connection_token.cancel();
                                    let _ = completion.send(Err(error));
                                }
                            }
                        }
                    }
                }
            }
        }

        connection_token.cancel();
        while let Some(joined) = timeout(STREAM_WORKER_DRAIN_TIMEOUT, workers.join_next())
            .await
            .ok()
            .flatten()
        {
            if let Err(error) = joined {
                debug!(?error, "Zakura stream worker failed during shutdown");
            }
        }
        workers.abort_all();
        if admitted_capabilities != 0 {
            self.registry.remove_peer(&peer_id, admitted_capabilities);
        }
        self.supervisor.deregister(&peer_id, remote_ip).await;
        metrics::counter!("zakura.p2p.conn.closed.neutral").increment(1);
        self.trace.emit(
            CONN_TABLE,
            conn.event("closed.neutral").reason(close_reason),
        );
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    async fn open_ordered_service_stream(
        &self,
        connection: &Connection,
        stream: Stream,
        workers: &mut JoinSet<()>,
        stream_sem: &Arc<Semaphore>,
        message_buckets: &mut MessageRateBuckets,
        limits: ZakuraConnectionLimits,
        per_stream_queue_depth: usize,
        connection_token: CancellationToken,
        freshness_tx: watch::Sender<Instant>,
        conn: ZakuraConnTrace,
        peer_id: ZakuraPeerId,
    ) -> Result<AdmittedOrderedStream, ZakuraHandlerError> {
        let stream_id = self.next_stream_id.fetch_add(1, Ordering::Relaxed);
        let permit = stream_sem
            .clone()
            .try_acquire_owned()
            .map_err(|_| ZakuraHandlerError::ResourceLimit("ordered stream permit"))?;
        let (mut send, recv) = timeout(OUTBOUND_STREAM_WRITE_TIMEOUT, connection.open_bi())
            .await
            .map_err(|_| ZakuraHandlerError::Timeout("open ordered service stream"))??;
        let prelude = StreamPrelude {
            magic: STREAM_PRELUDE_MAGIC,
            stream_kind: stream.kind,
            stream_version: stream.version,
            request_id: None,
            max_frame_bytes: app_frame_cap_for_stream_kind(&limits, stream.kind),
        };
        let prelude_bytes = prelude.encode()?;
        timeout(
            OUTBOUND_STREAM_WRITE_TIMEOUT,
            send.write_all(&prelude_bytes),
        )
        .await
        .map_err(|_| ZakuraHandlerError::Timeout("ordered stream prelude write"))??;

        let message_bucket = message_bucket_for(
            message_buckets,
            stream.kind,
            limits.message_rate_per_second,
            RealClock,
        );
        let stream_token = connection_token.child_token();
        let context = StreamWorkerContext {
            trace: self.trace.clone(),
            conn: conn.clone(),
            peer_id,
            stream_id,
            _permit: permit,
            limits,
            message_bucket,
            connection_token,
            stream_token,
            freshness_tx,
        };

        metrics::counter!(
            "zakura.p2p.stream.accepted",
            "stream_kind" => stream_kind_label(stream.kind),
        )
        .increment(1);
        self.trace.emit(
            STREAM_TABLE,
            conn.event("accepted")
                .stream(stream_id)
                .stream_kind(stream_kind_label(stream.kind)),
        );

        Ok(spawn_persistent_stream_worker(
            workers,
            send,
            recv,
            prelude,
            context,
            per_stream_queue_depth,
        ))
    }

    async fn admit_bi_stream(
        &self,
        mut send: SendStream,
        mut recv: RecvStream,
        admission: &mut StreamAdmission<'_>,
        per_stream_queue_depth: usize,
    ) -> Option<AdmittedOrderedStream> {
        let stream_id = self.next_stream_id.fetch_add(1, Ordering::Relaxed);
        let Ok(permit) = admission.stream_sem.clone().try_acquire_owned() else {
            let _ = send.reset(VarInt::from_u32(ZAKURA_CLOSE_RESOURCE));
            metrics::counter!("zakura.p2p.stream.rejected.semaphore").increment(1);
            admission.trace.emit(
                STREAM_TABLE,
                admission.event("rejected.semaphore", stream_id),
            );
            return None;
        };

        // Charge the per-connection stream-open rate token immediately after
        // acquiring a concurrency permit, before parsing the prelude or running
        // the kind/capability/mode checks below. Each of those rejections resets
        // the stream only and keeps the connection, so charging the token here is
        // what bounds protocol-invalid stream churn (bad prelude, unknown kind,
        // unnegotiated capability): otherwise an authenticated peer could open
        // such streams indefinitely without ever spending open-rate budget.
        if !admission.open_limiter.try_take() {
            let _ = send.reset(VarInt::from_u32(ZAKURA_CLOSE_RATE_LIMIT));
            metrics::counter!("zakura.p2p.stream.rejected.open_rate").increment(1);
            admission.trace.emit(
                STREAM_TABLE,
                admission.event("rejected.open_rate", stream_id),
            );
            return None;
        }

        let prelude = match read_stream_prelude(&mut recv, admission.limits.prelude_timeout).await {
            Ok(prelude) => prelude,
            Err(error) => {
                debug!(?error, "rejecting Zakura stream with bad prelude");
                let _ = send.reset(VarInt::from_u32(ZAKURA_CLOSE_BAD_PRELUDE));
                metrics::counter!("zakura.p2p.stream.rejected.prelude").increment(1);
                admission
                    .trace
                    .emit(STREAM_TABLE, admission.event("rejected.prelude", stream_id));
                return None;
            }
        };
        let stream_kind = stream_kind_label(prelude.stream_kind);

        let Some(stream) = self
            .registry
            .stream(prelude.stream_kind, prelude.stream_version)
        else {
            debug!(
                stream_kind = prelude.stream_kind,
                stream_version = prelude.stream_version,
                "rejecting Zakura stream with unknown kind or unsupported version"
            );
            let _ = send.reset(VarInt::from_u32(ZAKURA_CLOSE_UNKNOWN_STREAM));
            metrics::counter!(
                "zakura.p2p.stream.rejected.unknown_kind",
                "stream_kind" => stream_kind,
            )
            .increment(1);
            admission.trace.emit(
                STREAM_TABLE,
                admission
                    .event("rejected.unknown_kind", stream_id)
                    .stream_kind(stream_kind),
            );
            return None;
        };

        if admission.accepted_capabilities & stream.capability != stream.capability {
            debug!(
                stream_kind = prelude.stream_kind,
                stream_version = prelude.stream_version,
                accepted_capabilities = admission.accepted_capabilities,
                "rejecting Zakura stream that was not negotiated for this peer"
            );
            let _ = send.reset(VarInt::from_u32(ZAKURA_CLOSE_UNKNOWN_STREAM));
            metrics::counter!(
                "zakura.p2p.stream.rejected.unnegotiated_capability",
                "stream_kind" => stream_kind,
            )
            .increment(1);
            admission.trace.emit(
                STREAM_TABLE,
                admission
                    .event("rejected.unnegotiated_capability", stream_id)
                    .stream_kind(stream_kind),
            );
            return None;
        }

        if stream.mode != StreamMode::RequestResponse && prelude.request_id.is_some() {
            debug!("rejecting non-request Zakura stream with request id");
            let _ = send.reset(VarInt::from_u32(ZAKURA_CLOSE_BAD_PRELUDE));
            admission.connection_token.cancel();
            metrics::counter!("zakura.p2p.stream.rejected.unexpected_request_id").increment(1);
            admission.trace.emit(
                STREAM_TABLE,
                admission
                    .event("rejected.unexpected_request_id", stream_id)
                    .stream_kind(stream_kind),
            );
            return None;
        }

        if stream.mode == StreamMode::RequestResponse && prelude.request_id.is_none() {
            debug!("rejecting Zakura request stream without request id");
            let _ = send.reset(VarInt::from_u32(ZAKURA_CLOSE_BAD_PRELUDE));
            admission.connection_token.cancel();
            metrics::counter!("zakura.p2p.stream.rejected.request_without_id").increment(1);
            admission.trace.emit(
                STREAM_TABLE,
                admission
                    .event("rejected.request_without_id", stream_id)
                    .stream_kind(stream_kind),
            );
            return None;
        }

        metrics::counter!(
            "zakura.p2p.stream.accepted",
            "stream_kind" => stream_kind,
        )
        .increment(1);
        admission.trace.emit(
            STREAM_TABLE,
            admission
                .event("accepted", stream_id)
                .stream_kind(stream_kind),
        );

        let message_bucket = message_bucket_for(
            admission.message_buckets,
            prelude.stream_kind,
            admission.limits.message_rate_per_second,
            RealClock,
        );
        let stream_token = admission.connection_token.child_token();

        let context = StreamWorkerContext {
            trace: admission.trace.clone(),
            conn: admission.conn.clone(),
            peer_id: admission.peer_id.clone(),
            stream_id,
            _permit: permit,
            limits: admission.limits,
            message_bucket,
            connection_token: admission.connection_token.clone(),
            stream_token,
            freshness_tx: admission.freshness_tx.clone(),
        };

        if stream.mode == StreamMode::RequestResponse {
            admission.workers.spawn(request_stream_worker(
                send,
                recv,
                prelude,
                context,
                self.registry.clone(),
            ));
            None
        } else {
            Some(spawn_persistent_stream_worker(
                admission.workers,
                send,
                recv,
                prelude,
                context,
                per_stream_queue_depth,
            ))
        }
    }

    async fn register_and_serve(
        &self,
        connection: Connection,
        peer_id: ZakuraPeerId,
        remote_ip: Option<IpAddr>,
        context: ConnectionServeContext,
    ) -> Result<(), ZakuraHandlerError> {
        let ordered_stream_count = if context.role == "initiator" {
            self.registry
                .ordered_streams_for_escalation(
                    context.accepted_capabilities,
                    &peer_id,
                    context.direction,
                )
                .len()
        } else {
            self.registry
                .ordered_streams_for_negotiated(context.accepted_capabilities)
                .len()
        };
        if ordered_stream_count > usize::from(context.limits.max_open_streams) {
            debug!(
                max_open_streams = context.limits.max_open_streams,
                ordered_stream_count,
                "rejecting Zakura peer before registration because negotiated ordered streams exceed max-open-streams"
            );
            connection.close(VarInt::from_u32(ZAKURA_CLOSE_RESOURCE), b"ordered streams");
            return Ok(());
        }
        if ordered_stream_count > 0
            && usize::from(context.limits.max_inbound_queue_depth) < ordered_stream_count
        {
            debug!(
                max_inbound_queue_depth = context.limits.max_inbound_queue_depth,
                ordered_stream_count,
                "rejecting Zakura peer before registration because inbound queue depth cannot be split"
            );
            connection.close(VarInt::from_u32(ZAKURA_CLOSE_RESOURCE), b"queue split");
            return Ok(());
        }

        let (outbound_tx, outbound_rx) =
            mpsc::channel(usize::from(context.limits.max_inbound_queue_depth));
        let outbound_handle = ZakuraPeerHandle {
            peer_id: peer_id.clone(),
            sender: outbound_tx,
        };
        let connection_token = self.shutdown.child_token();
        let registration = self
            .supervisor
            .register(
                peer_id,
                remote_ip,
                context.transcript_hash,
                outbound_handle,
                connection_token.clone(),
                context.accepted_capabilities,
            )
            .await;

        match registration {
            ZakuraRegistration::Registered {
                peer_id,
                remote_ip,
                disconnect_token,
            } => {
                metrics::counter!("zakura.p2p.conn.accepted", "role" => context.role).increment(1);
                self.trace.emit(
                    CONN_TABLE,
                    context
                        .conn
                        .event("accepted")
                        .role(context.role)
                        .direction(context.direction.trace_label()),
                );
                self.serve_connection(
                    connection,
                    peer_id,
                    remote_ip,
                    outbound_rx,
                    RegisteredConnectionServeContext {
                        limits: context.limits,
                        conn: context.conn,
                        connection_token: disconnect_token,
                        accepted_capabilities: context.accepted_capabilities,
                        opens_ordered_streams: context.role == "initiator",
                        direction: context.direction,
                    },
                )
                .await
            }
            ZakuraRegistration::Duplicate { peer_id } => {
                debug!(?peer_id, "closing duplicate Zakura peer neutrally");
                metrics::counter!("zakura.p2p.conn.duplicate").increment(1);
                self.trace.emit(
                    CONN_TABLE,
                    context
                        .conn
                        .event("duplicate")
                        .role(context.role)
                        .direction(context.direction.trace_label()),
                );
                connection.close(VarInt::from_u32(ZAKURA_CLOSE_NEUTRAL), b"duplicate");
                Ok(())
            }
            ZakuraRegistration::Rejected(reason) => {
                debug!(
                    ?reason,
                    "closing Zakura peer neutrally after registration rejection"
                );
                self.trace.emit(
                    CONN_TABLE,
                    context
                        .conn
                        .event("rejected.admission")
                        .role(context.role)
                        .direction(context.direction.trace_label())
                        .reason(reject_reason_label(reason)),
                );
                connection.close(VarInt::from_u32(ZAKURA_CLOSE_RESOURCE), b"registration");
                Ok(())
            }
        }
    }
}

impl ProtocolHandler for ZakuraProtocolHandler {
    async fn accept(&self, connection: Connection) -> Result<(), AcceptError> {
        self.accept_connection(connection).await
    }

    async fn shutdown(&self) {
        self.shutdown.cancel();
    }
}

/// Loopback IPv4 address the native Zakura endpoint binds to when no
/// `zakura.listen_addr` is configured, so the unset (dial-out-only) state does
/// not expose the P2P_V2_ALPN surface on all interfaces.
const ZAKURA_LOOPBACK_BIND_V4: SocketAddrV4 = SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0);
/// Loopback IPv6 counterpart of [`ZAKURA_LOOPBACK_BIND_V4`].
const ZAKURA_LOOPBACK_BIND_V6: SocketAddrV6 = SocketAddrV6::new(Ipv6Addr::LOCALHOST, 0, 0, 0);

/// Applies the native endpoint bind-address selection to `builder`.
///
/// When `listen_addr` is set, the endpoint binds exactly that address so the
/// node has a stable, advertisable Zakura endpoint. When it is unset, the
/// endpoint binds loopback-only for **both** IPv4 and IPv6 rather than relying
/// on iroh's default unspecified bind (`0.0.0.0:0` / `[::]:0`). Without the
/// explicit loopback bind the unconfigured / dial-out-only case silently exposes
/// the experimental native P2P_V2_ALPN handshake/session surface on every
/// interface on an OS-assigned ephemeral port.
fn bind_native_endpoint(
    builder: iroh::endpoint::Builder,
    listen_addr: Option<SocketAddr>,
) -> iroh::endpoint::Builder {
    match listen_addr {
        Some(SocketAddr::V4(addr)) => builder.bind_addr_v4(addr),
        Some(SocketAddr::V6(addr)) => builder.bind_addr_v6(addr),
        None => builder
            .bind_addr_v4(ZAKURA_LOOPBACK_BIND_V4)
            .bind_addr_v6(ZAKURA_LOOPBACK_BIND_V6),
    }
}

/// Start a Zakura endpoint and router when P2P v2 is enabled.
pub async fn spawn_zakura_endpoint(
    config: &Config,
    sink_factory: impl FnOnce(ZakuraSupervisorHandle, ZakuraTrace) -> Arc<dyn Service>,
) -> Result<Option<ZakuraEndpoint>, BoxError> {
    spawn_zakura_endpoint_with_header_sync_driver(config, sink_factory, None).await
}

/// Start a Zakura endpoint with an externally driven header-sync reactor.
pub async fn spawn_zakura_endpoint_with_header_sync_driver(
    config: &Config,
    sink_factory: impl FnOnce(ZakuraSupervisorHandle, ZakuraTrace) -> Arc<dyn Service>,
    header_sync_driver_startup: Option<ZakuraHeaderSyncDriverStartup>,
) -> Result<Option<ZakuraEndpoint>, BoxError> {
    if !config.v2_p2p {
        return Ok(None);
    }

    let limits = ZakuraLocalLimits::from_config(config);
    validate_idle_invariant(&limits)?;
    let secret_key = zakura_secret_key(config)?;
    let discovery_secret_key = secret_key.clone();
    let builder = direct_endpoint_builder(secret_key).transport_config(limits.transport_config());
    // Bind a fixed address when configured so this node has a stable, advertisable
    // Zakura endpoint; otherwise bind loopback-only so the unset (dial-out-only)
    // case does not expose the native P2P_V2_ALPN surface on all interfaces.
    let builder = bind_native_endpoint(builder, config.zakura.listen_addr);
    let endpoint = builder.bind().await?;
    let supervisor = ZakuraSupervisorHandle::new(config.max_connections_per_ip);
    let tracer = config
        .zakura
        .trace_dir
        .clone()
        .map(zebra_jsonl_trace::JsonlTracer::spawn)
        .unwrap_or_else(zebra_jsonl_trace::JsonlTracer::noop);
    let trace = ZakuraTrace::new(tracer, zebra_jsonl_trace::node_id());
    let handshake_config = ZakuraHandshakeConfig::for_network(&config.network);
    let discovery = super::discovery::build_discovery_handle(
        discovery_secret_key,
        config.zakura.listen_addr.into_iter().collect(),
        super::discovery::default_advertised_services(),
        &handshake_config,
        config.zakura.max_connections,
        config.zakura.bootstrap_peers.len(),
        supervisor.subscribe(),
    )?;
    let anchor = config.zakura.header_sync.anchor(&config.network)?;
    let frontiers = header_sync_driver_startup.as_ref().map_or(
        HeaderSyncFrontiers {
            finalized_height: anchor.0,
            verified_block_tip: anchor.0,
            verified_block_hash: anchor.1,
        },
        |startup| startup.frontiers,
    );
    let best_header_tip = header_sync_driver_startup
        .as_ref()
        .map_or(Some(anchor), |startup| startup.best_header_tip);
    let sync_frontier = header_sync_driver_startup.as_ref().map(|driver_startup| {
        let best_header_tip = driver_startup.best_header_tip.unwrap_or(anchor);
        let initial = FrontierUpdate {
            frontier: crate::zakura::chain_frontier_from_parts(
                driver_startup.frontiers.finalized_height,
                Frontier::new(
                    driver_startup.frontiers.verified_block_tip,
                    driver_startup.verified_block_tip_hash,
                ),
                Frontier::new(best_header_tip.0, best_header_tip.1),
            ),
            change: FrontierChange::Snapshot,
        };
        ZakuraSyncExchange::new(initial, trace.clone())
    });
    let mut startup = HeaderSyncStartup::new(
        config.network.clone(),
        anchor,
        frontiers,
        best_header_tip,
        config.zakura.header_sync.clone(),
        limits.max_frame_bytes,
    );
    startup.status_refresh_interval = config.zakura.header_sync.status_refresh_interval;
    startup.trace = trace.clone();
    startup.frontier_updates = sync_frontier
        .as_ref()
        .map(ZakuraSyncExchange::subscribe_frontier);
    let header_sync_shutdown = CancellationToken::new();
    startup.shutdown = header_sync_shutdown.clone();
    if header_sync_driver_startup.is_some() {
        startup.range_state_actions_enabled = true;
        startup.inbound_new_block_acceptance_enabled = config.zakura.header_sync.accept_new_blocks;
    }
    let (header_sync, header_sync_actions, header_sync_task) = spawn_header_sync_reactor(startup)?;
    let block_sync_driver_enabled = header_sync_driver_startup.is_some();
    let (block_sync, block_sync_actions, block_sync_task) =
        if let Some(driver_startup) = header_sync_driver_startup.as_ref() {
            let best_header_tip = driver_startup.best_header_tip.unwrap_or(anchor);
            let frontier_updates = sync_frontier
                .as_ref()
                .expect("sync frontier is initialized when block sync driver is enabled")
                .subscribe_frontier();
            let mut startup = BlockSyncStartup::new_with_exchange(
                BlockSyncFrontiers {
                    finalized_height: driver_startup.frontiers.finalized_height,
                    verified_block_tip: driver_startup.frontiers.verified_block_tip,
                    verified_block_hash: driver_startup.verified_block_tip_hash,
                },
                best_header_tip,
                frontier_updates,
                config.zakura.block_sync.clone(),
            );
            startup.shutdown = header_sync_shutdown.clone();
            startup.trace = trace.clone();
            let (handle, actions, task) = spawn_block_sync_reactor(startup);
            (Some(handle), Some(actions), Some(task))
        } else {
            (None, None, None)
        };
    let discovery_service = Arc::new(super::DiscoveryService::with_sync_services(
        discovery.clone(),
        header_sync.clone(),
        block_sync.clone(),
    )) as Arc<dyn Service>;
    let legacy_service = sink_factory(supervisor.clone(), trace.clone());
    let registry = service_registry(
        &supervisor,
        Some(header_sync.clone()),
        block_sync.clone(),
        config.zakura.block_sync.clone(),
        legacy_service,
        discovery_service,
    )?;
    let mut tasks = vec![header_sync_task];
    if let Some(task) = block_sync_task {
        tasks.push(task);
    }
    let header_sync_actions = if block_sync_driver_enabled {
        Some(Arc::new(Mutex::new(Some(header_sync_actions))))
    } else {
        let action_driver_task = tokio::spawn(drive_header_sync_actions(
            header_sync_actions,
            header_sync.clone(),
            supervisor.clone(),
            header_sync_shutdown.clone(),
        ));
        tasks.push(action_driver_task);
        None
    };
    let block_sync_actions = block_sync_actions
        .filter(|_| block_sync_driver_enabled)
        .map(|actions| Arc::new(Mutex::new(Some(actions))));
    let header_sync_tasks = Arc::new(HeaderSyncBackgroundTasks {
        shutdown: header_sync_shutdown,
        tasks: Mutex::new(tasks),
    });
    let handler = ZakuraProtocolHandler::new_with_registry_and_trace(
        supervisor.clone(),
        config.network.clone(),
        handshake_config,
        limits.clone(),
        registry,
        trace,
    )
    // Give the handler the bound endpoint so inbound accepts can resolve the
    // peer's source IP and enforce the per-IP connection cap.
    .with_endpoint(endpoint.clone());
    let router = Router::builder(endpoint)
        .accept(P2P_V2_ALPN, handler.clone())
        .spawn();
    let endpoint = ZakuraEndpoint {
        router,
        supervisor,
        handler,
        header_sync: Some(header_sync),
        block_sync,
        sync_frontier,
        header_sync_tasks: Some(header_sync_tasks),
        header_sync_actions,
        block_sync_actions,
        upgrade_dials: Arc::new(StdMutex::new(HashMap::new())),
    };

    // Log our own dial address once iroh has resolved it, so operators can hand
    // out `<node_id>@<direct_addr>` for other nodes' `zakura.bootstrap_peers`.
    // Tracked under the endpoint shutdown owner and shutdown-aware so it cannot
    // outlive endpoint teardown while awaiting address resolution.
    {
        let shutdown = endpoint.background_shutdown_token();
        let log_endpoint = endpoint.clone();
        let task = tokio::spawn(async move {
            tokio::select! {
                biased;
                _ = shutdown.cancelled() => {}
                node_addr = log_endpoint.node_addr() => {
                    let direct_addresses: Vec<String> = node_addr
                        .direct_addresses()
                        .map(|addr| addr.to_string())
                        .collect();
                    info!(
                        node_id = %node_addr.node_id,
                        ?direct_addresses,
                        "Zakura P2P endpoint ready; advertise <node_id>@<direct_addr> as a bootstrap peer",
                    );
                }
            }
        });
        endpoint.push_header_sync_task(task).await;
    }

    super::discovery::insert_static_bootstrap_candidates(
        &discovery,
        &config.zakura.bootstrap_peers,
    )
    .await;
    // Track the maintained bootstrap redial tasks and the discovery candidate
    // dialer under the endpoint shutdown owner so `ZakuraEndpoint::shutdown`
    // drains them. Previously their handles were dropped, leaving detached loops
    // that kept retrying dials against a torn-down router while holding endpoint
    // clones past teardown.
    for task in spawn_native_bootstrap_dialer(
        endpoint.clone(),
        config.zakura.bootstrap_peers.clone(),
        limits.clone(),
    ) {
        endpoint.push_header_sync_task(task).await;
    }
    let discovery_dialer =
        super::discovery::spawn_native_discovery_dialer(endpoint.clone(), discovery, limits);
    endpoint.push_header_sync_task(discovery_dialer).await;
    Ok(Some(endpoint))
}

pub(crate) async fn serve_native_dial_connection(
    endpoint: &ZakuraEndpoint,
    node_addr: NodeAddr,
    limits: &ZakuraLocalLimits,
) -> Result<(), ZakuraHandlerError> {
    let conn_id = endpoint
        .handler
        .next_conn_id
        .fetch_add(1, Ordering::Relaxed);
    let _admission = endpoint
        .handler
        .admission
        .clone()
        .try_acquire_owned()
        .map_err(|_| ZakuraHandlerError::ResourceLimit("admission"))?;
    let remote_ip = node_addr.direct_addresses().next().map(|addr| addr.ip());
    let connection = timeout(
        limits.control_timeout,
        endpoint.router.endpoint().connect(node_addr, P2P_V2_ALPN),
    )
    .await
    .map_err(|_| ZakuraHandlerError::Timeout("native dial"))??;
    let remote_node_id = connection.remote_node_id()?;
    let peer_id = ZakuraPeerId::new(remote_node_id.as_bytes().to_vec())?;
    let conn = ZakuraConnTrace::new(&endpoint.handler.trace, conn_id, &peer_id);
    let local_node_id = endpoint.router.endpoint().node_id();
    let local_peer_id = ZakuraPeerId::new(local_node_id.as_bytes().to_vec())?;
    let negotiated = {
        let _handshake = endpoint
            .handler
            .pending_handshakes
            .clone()
            .try_acquire_owned()
            .map_err(|_| ZakuraHandlerError::ResourceLimit("pending handshake"))?;
        run_native_initiator_handshake(
            &connection,
            limits,
            &endpoint.handler.handshake_config,
            &local_peer_id,
            &endpoint.handler.trace,
            &conn,
        )
        .await?
    };
    let conn_limits = limits.clamp(&negotiated.limits);
    let direction = ServicePeerDirection::Outbound;
    endpoint
        .handler
        .register_and_serve(
            connection,
            peer_id,
            remote_ip,
            ConnectionServeContext {
                limits: conn_limits,
                accepted_capabilities: negotiated.accepted_capabilities,
                role: "initiator",
                direction,
                transcript_hash: native_connection_transcript_hash(
                    direction,
                    &local_node_id,
                    &remote_node_id,
                ),
                conn,
            },
        )
        .await
}

#[cfg(any(test, feature = "zakura-testkit"))]
pub(crate) async fn run_native_initiator_handshake_without_trace(
    connection: &Connection,
    limits: &ZakuraLocalLimits,
    handshake_config: &ZakuraHandshakeConfig,
    local_peer_id: &ZakuraPeerId,
) -> Result<NativeHandshakeNegotiated, ZakuraHandlerError> {
    run_native_initiator_handshake(
        connection,
        limits,
        handshake_config,
        local_peer_id,
        &ZakuraTrace::noop(),
        &ZakuraConnTrace::placeholder(),
    )
    .await
}

async fn run_native_initiator_handshake(
    connection: &Connection,
    limits: &ZakuraLocalLimits,
    handshake_config: &ZakuraHandshakeConfig,
    local_peer_id: &ZakuraPeerId,
    trace: &ZakuraTrace,
    conn: &ZakuraConnTrace,
) -> Result<NativeHandshakeNegotiated, ZakuraHandlerError> {
    trace.emit(
        HANDSHAKE_TABLE,
        conn.event("control.started")
            .role("initiator")
            .phase("control")
            .network(handshake_config.network_label()),
    );
    let (mut send, mut recv) = timeout(limits.control_timeout, connection.open_bi())
        .await
        .map_err(|_| ZakuraHandlerError::Timeout("open control stream"))??;
    let mut local_nonce = [0; 32];
    OsRng.fill_bytes(&mut local_nonce);

    let hello = ZakuraControlHello {
        magic: CONTROL_HELLO_MAGIC,
        control_version: CONTROL_VERSION,
        selected_zakura_protocol: ZAKURA_PROTOCOL_VERSION_1,
        handshake_path: ZakuraHandshakePath::Native,
        role: ZakuraControlRole::Initiator,
        network_id: handshake_config.network_id,
        chain_id: handshake_config.chain_id,
        iroh_node_id: local_peer_id.as_bytes().to_vec(),
        peer_nonce: local_nonce,
        initiator_upgrade_nonce: [0; 32],
        responder_upgrade_nonce: [0; 32],
        legacy_upgrade_transcript: [0; 32],
        capabilities: handshake_config.supported_capabilities,
        required_channels: 0,
        initial_limits: limits.initial_limits(),
    };

    write_control_payload(&mut send, &hello.encode()?, limits.control_timeout).await?;
    let ack_bytes = read_control_payload(
        &mut recv,
        handshake_config.max_control_frame_bytes,
        limits.control_timeout,
    )
    .await?;
    let ack = ZakuraControlAck::decode(&ack_bytes)?;
    ack.validate(
        ZAKURA_PROTOCOL_VERSION_1,
        local_nonce,
        ack.peer_nonce,
        &limits.initial_limits(),
        handshake_config,
    )?;
    trace.emit(
        HANDSHAKE_TABLE,
        conn.event("control.succeeded")
            .role("initiator")
            .phase("control")
            .selected_protocol(ack.selected_zakura_protocol)
            .network(handshake_config.network_label()),
    );
    Ok(NativeHandshakeNegotiated {
        limits: ack.accepted_limits,
        accepted_capabilities: ack.accepted_capabilities,
    })
}

fn spawn_persistent_stream_worker(
    workers: &mut JoinSet<()>,
    send: SendStream,
    recv: RecvStream,
    prelude: StreamPrelude,
    context: StreamWorkerContext,
    queue_depth: usize,
) -> AdmittedOrderedStream {
    let (to_service_tx, to_service_rx) = mpsc::channel(queue_depth);
    let (from_service_tx, from_service_rx) = mpsc::channel(queue_depth);
    let admitted = AdmittedOrderedStream {
        kind: prelude.stream_kind,
        recv: FramedRecv::new(to_service_rx),
        send: FramedSend::new(from_service_tx),
        cancel_token: context.stream_token.clone(),
    };

    workers.spawn(persistent_stream_worker(
        send,
        recv,
        prelude,
        context,
        to_service_tx,
        from_service_rx,
        queue_depth,
    ));

    admitted
}

async fn persistent_stream_worker(
    mut send: SendStream,
    recv: RecvStream,
    prelude: StreamPrelude,
    context: StreamWorkerContext,
    inbound_tx: mpsc::Sender<Frame>,
    outbound_rx: mpsc::Receiver<Frame>,
    queue_depth_limit: usize,
) {
    let context = Arc::new(context);
    let stream_kind = prelude.stream_kind;

    // The inbound reader runs in its own task, not as a `select!` branch racing
    // the outbound writer below. `read_frame` is NOT cancellation-safe: it
    // consumes the fixed frame header, then awaits the (multi-packet) payload. If
    // it shared this `select!` with the outbound arm, an outbound frame becoming
    // ready mid-read would drop the `read_frame` future and discard the header
    // bytes it already consumed, desyncing the stream forever -- the next read
    // decodes body bytes as a header, yielding a garbage multi-GiB `payload_len`,
    // an `OversizeFrame` error, and a stream reset. Heavy concurrent body-sync
    // (inbound bodies + outbound `GetBlocks`) made this fire constantly. Reading
    // in a dedicated task removes the write/read race; the main loop only ever
    // *receives* fully-read frames over a channel, which is cancellation-safe.
    let (frame_tx, mut frame_rx) = mpsc::channel::<Result<Frame, ZakuraHandlerError>>(1);
    let reader_context = Arc::clone(&context);
    let reader = tokio::spawn(async move {
        let mut recv = recv;
        loop {
            let frame = tokio::select! {
                biased;
                _ = reader_context.connection_token.cancelled() => break,
                _ = reader_context.stream_token.cancelled() => break,
                frame = read_frame(
                    &mut recv,
                    inbound_frame_cap_for_stream_kind(&reader_context.limits, stream_kind),
                    reader_context.limits.idle_timeout,
                ) => frame,
            };
            // Admit (rate/oversize) at ingress, the instant a frame is read, so
            // throttling never trails behind the main loop draining queued
            // outbound writes or forwarding an earlier frame to a service that
            // might disconnect first. The main loop only ever receives frames
            // that already cleared admission, plus terminal errors it maps to a
            // reset code (it owns the send half). Admission is charged exactly
            // once, here.
            let message = match frame {
                Ok(frame) => {
                    let _ = reader_context.freshness_tx.send(Instant::now());
                    match admit_inbound_message(frame.payload.len(), &reader_context, stream_kind) {
                        InboundMessageAdmission::Admit => Ok(frame),
                        InboundMessageAdmission::Oversize => Err(ZakuraHandlerError::Oversize),
                        InboundMessageAdmission::Throttled => Err(ZakuraHandlerError::RateLimited),
                    }
                }
                Err(error) => {
                    // Emit the oversize-desync diagnostic here, at the read, so
                    // it is recorded even if the main loop tears the worker down
                    // for an outbound write that stopped first.
                    if let Some((payload_len, frame_len, max_frame_bytes)) =
                        error.oversize_frame_details()
                    {
                        reader_context.trace.emit(
                            RATELIMIT_TABLE,
                            reader_context
                                .event("frame.oversize")
                                .stream_kind(stream_kind_label(stream_kind))
                                .payload_len(payload_len)
                                .frame_len(frame_len)
                                .max_frame_bytes(max_frame_bytes),
                        );
                    }
                    Err(error)
                }
            };
            // Any error is terminal. `Closed` is a clean peer-initiated close;
            // every other error is a protocol/limit violation that must
            // disconnect the peer. Forward the error first so the main loop can
            // map it to a reset code, then cancel the connection ourselves so
            // the disconnect is guaranteed even if the main loop tore the worker
            // down for a stopped outbound write before processing it.
            let is_terminal = message.is_err();
            let must_disconnect =
                matches!(&message, Err(error) if !matches!(error, ZakuraHandlerError::Closed));
            let forward_failed = frame_tx.send(message).await.is_err();
            if must_disconnect {
                reader_context.connection_token.cancel();
            }
            if forward_failed || is_terminal {
                break;
            }
        }
    });

    let mut outbound_rx = Some(outbound_rx);
    loop {
        tokio::select! {
            biased;
            _ = context.connection_token.cancelled() => break,
            _ = context.stream_token.cancelled() => break,
            outbound = async {
                match outbound_rx.as_mut() {
                    Some(outbound_rx) => outbound_rx.recv().await,
                    None => future::pending().await,
                }
            } => {
                match outbound {
                    Some(frame) => {
                        if let Err(error) = write_ordered_frame(&mut send, frame, context.limits, stream_kind).await {
                            if ordered_stream_write_was_stopped(&error) {
                                debug!(?error, "closing Zakura ordered stream after peer stopped receiving");
                                break;
                            }
                            debug!(?error, "closing Zakura ordered stream writer");
                            let _ = send.reset(VarInt::from_u32(ZAKURA_CLOSE_BAD_PRELUDE));
                            context.connection_token.cancel();
                            break;
                        }
                    }
                    None => {
                        outbound_rx = None;
                    }
                }
            }
            inbound = frame_rx.recv() => {
                match inbound {
                    // Frames here already cleared ingress admission in the reader.
                    Some(Ok(frame)) => {
                        if inbound_tx.send(frame).await.is_err() {
                            debug!(
                                stream_kind,
                                "closing Zakura ordered stream after local service receiver dropped"
                            );
                            break;
                        }
                        metrics::gauge!(
                            "zakura.p2p.queue.depth",
                            "stream_kind" => stream_kind_label(stream_kind),
                        )
                        .set(queue_depth_limit.saturating_sub(inbound_tx.capacity()) as f64);
                    }
                    // The reader signalled an oversize message: disconnect it.
                    Some(Err(ZakuraHandlerError::Oversize)) => {
                        let _ = send.reset(VarInt::from_u32(ZAKURA_CLOSE_OVERSIZE));
                        context.connection_token.cancel();
                        break;
                    }
                    Some(Err(ZakuraHandlerError::RateLimited)) => {
                        let _ = send.reset(VarInt::from_u32(ZAKURA_CLOSE_RATE_LIMIT));
                        context.connection_token.cancel();
                        break;
                    }
                    Some(Err(ZakuraHandlerError::Closed)) | None => {
                        break;
                    }
                    // The reader already emitted any oversize-desync diagnostic.
                    Some(Err(error)) => {
                        debug!(?error, "closing Zakura stream worker");
                        let _ = send.reset(VarInt::from_u32(ZAKURA_CLOSE_BAD_PRELUDE));
                        context.connection_token.cancel();
                        break;
                    }
                }
            }
        }
    }

    // Stop the reader: it also observes the cancellation tokens, but abort
    // guarantees a prompt exit on the paths that break without cancelling one
    // (e.g. a peer that stopped receiving, or the local service receiver closing).
    reader.abort();
}

fn ordered_stream_write_was_stopped(error: &BoxError) -> bool {
    matches!(
        error.downcast_ref::<iroh::endpoint::WriteError>(),
        Some(iroh::endpoint::WriteError::Stopped(_))
    )
}

async fn request_stream_worker(
    mut send: SendStream,
    mut recv: RecvStream,
    prelude: StreamPrelude,
    context: StreamWorkerContext,
    registry: Arc<ServiceRegistry>,
) {
    let Some(request_id) = prelude.request_id else {
        let _ = send.reset(VarInt::from_u32(ZAKURA_CLOSE_BAD_PRELUDE));
        context.connection_token.cancel();
        return;
    };

    let frame = tokio::select! {
        biased;
        _ = context.connection_token.cancelled() => return,
        frame = read_frame(
            &mut recv,
            inbound_frame_cap_for_stream_kind(&context.limits, prelude.stream_kind),
            context.limits.idle_timeout,
        ) => frame,
    };

    let frame = match frame {
        Ok(frame) => frame,
        Err(error) => {
            debug!(
                ?error,
                "closing Zakura request stream with invalid request frame"
            );
            let _ = send.reset(VarInt::from_u32(ZAKURA_CLOSE_BAD_PRELUDE));
            context.connection_token.cancel();
            return;
        }
    };

    let _ = context.freshness_tx.send(Instant::now());
    match admit_inbound_message(frame.payload.len(), &context, prelude.stream_kind) {
        InboundMessageAdmission::Admit => {}
        InboundMessageAdmission::Oversize => {
            let _ = send.reset(VarInt::from_u32(ZAKURA_CLOSE_OVERSIZE));
            context.connection_token.cancel();
            return;
        }
        InboundMessageAdmission::Throttled => {
            // The admission path already counted/traced the over-rate frame.
            // Do not reject request streams here: request/response peers also
            // will not resend a dropped frame, and backpressure is safer than
            // data loss while block sync is catching up.
        }
    }

    let response_frames = match registry
        .request(
            context.peer_id.clone(),
            prelude.stream_kind,
            request_id,
            app_frame_cap_for_stream_kind(&context.limits, prelude.stream_kind),
            context.limits.max_message_bytes,
            frame,
        )
        .await
    {
        Ok(frames) => frames,
        Err(SinkReject::Protocol(error)) => {
            debug!(
                ?error,
                "Zakura inbound sink rejected protocol-invalid request"
            );
            let _ = send.reset(VarInt::from_u32(ZAKURA_CLOSE_BAD_PRELUDE));
            context.connection_token.cancel();
            return;
        }
        Err(SinkReject::Local(error)) => {
            debug!(
                ?error,
                "Zakura inbound sink could not answer request locally"
            );
            let _ = send.reset(VarInt::from_u32(ZAKURA_CLOSE_RESOURCE));
            return;
        }
    };

    for frame in response_frames {
        if let Err(error) = write_response_frame(&mut send, frame, context.limits).await {
            debug!(?error, "failed to write Zakura request response frame");
            let _ = send.reset(VarInt::from_u32(ZAKURA_CLOSE_BAD_PRELUDE));
            return;
        }
    }

    let _ = send.finish();
}

async fn freshness_reaper(mut freshness_rx: watch::Receiver<Instant>, idle_timeout: Duration) {
    loop {
        let last = *freshness_rx.borrow_and_update();
        let elapsed = last.elapsed();
        if elapsed >= idle_timeout {
            return;
        }
        tokio::select! {
            _ = tokio::time::sleep(idle_timeout - elapsed) => return,
            changed = freshness_rx.changed() => {
                if changed.is_err() {
                    return;
                }
            }
        }
    }
}

async fn read_stream_prelude(
    recv: &mut RecvStream,
    prelude_timeout: Duration,
) -> Result<StreamPrelude, ZakuraHandlerError> {
    let mut fixed = [0; STREAM_PRELUDE_FIXED_BYTES];
    timeout(prelude_timeout, recv.read_exact(&mut fixed))
        .await
        .map_err(|_| ZakuraHandlerError::Timeout("stream prelude"))??;

    let mut fixed_reader = &fixed[..];
    let mut magic = [0; 4];
    fixed_reader.read_exact(&mut magic)?;
    if magic != STREAM_PRELUDE_MAGIC {
        return Err(ZakuraProtocolError::InvalidMagic.into());
    }
    let stream_kind = fixed_reader.read_u16::<LittleEndian>()?;
    let stream_version = fixed_reader.read_u16::<LittleEndian>()?;
    let request_id = match fixed[STREAM_PRELUDE_REQUEST_ID_FLAG_OFFSET] {
        0 => None,
        1 => {
            let mut bytes = [0; STREAM_PRELUDE_REQUEST_ID_BYTES];
            timeout(prelude_timeout, recv.read_exact(&mut bytes))
                .await
                .map_err(|_| ZakuraHandlerError::Timeout("stream request id"))??;
            let mut request_id_reader = &bytes[..];
            Some(request_id_reader.read_u64::<LittleEndian>()?)
        }
        flag => return Err(ZakuraProtocolError::InvalidFlag(flag).into()),
    };

    let mut cap = [0; STREAM_PRELUDE_CAP_BYTES];
    timeout(prelude_timeout, recv.read_exact(&mut cap))
        .await
        .map_err(|_| ZakuraHandlerError::Timeout("stream prelude cap"))??;
    let mut cap_reader = &cap[..];
    let max_frame_bytes = cap_reader.read_u32::<LittleEndian>()?;

    Ok(StreamPrelude {
        magic,
        stream_kind,
        stream_version,
        request_id,
        max_frame_bytes,
    })
}

async fn read_frame(
    recv: &mut RecvStream,
    max_frame_bytes: u32,
    read_timeout: Duration,
) -> Result<Frame, ZakuraHandlerError> {
    let mut header = [0; FRAME_HEADER_BYTES];
    match timeout(read_timeout, recv.read_exact(&mut header)).await {
        Ok(Ok(())) => {}
        Ok(Err(_)) => return Err(ZakuraHandlerError::Closed),
        Err(_) => return Err(ZakuraHandlerError::Timeout("frame header")),
    }
    let mut reader = &header[..];
    let message_type = reader.read_u16::<LittleEndian>()?;
    let flags = reader.read_u16::<LittleEndian>()?;
    let payload_len = usize::try_from(reader.read_u32::<LittleEndian>()?)
        .expect("u32 payload lengths fit usize on supported targets");
    let max_frame_bytes =
        usize::try_from(max_frame_bytes).expect("u32 frame cap fits usize on supported targets");
    let frame_len = FRAME_HEADER_BYTES.saturating_add(payload_len);
    if frame_len > max_frame_bytes {
        metrics::counter!("zakura.p2p.ratelimit.frame.oversize").increment(1);
        return Err(ZakuraHandlerError::OversizeFrame {
            payload_len,
            frame_len,
            max_frame_bytes,
        });
    }
    let mut payload = vec![0; payload_len];
    timeout(read_timeout, recv.read_exact(&mut payload))
        .await
        .map_err(|_| ZakuraHandlerError::Timeout("frame payload"))??;
    Ok(Frame {
        message_type,
        flags,
        payload,
    })
}

async fn read_control_payload(
    recv: &mut RecvStream,
    max_bytes: u32,
    read_timeout: Duration,
) -> Result<Vec<u8>, ZakuraHandlerError> {
    // Control payloads (Hello/Ack) carry a hard 16 KiB cap that is otherwise only
    // enforced later in ZakuraControlHello/Ack::decode. Clamp the caller-supplied
    // frame cap (the configured `max_control_frame_bytes`, 1 MiB by default) to it
    // here so a peer cannot make us allocate/read a payload up to the much larger
    // frame cap before decode rejects it as oversized.
    //
    // `MAX_CONTROL_PAYLOAD_BYTES` (16 KiB) is a small compile-time constant, so the
    // cast to u32 cannot truncate; `.min` also preserves any tighter negotiated cap.
    let max_bytes = max_bytes.min(MAX_CONTROL_PAYLOAD_BYTES as u32);
    let mut len_bytes = [0; CONTROL_LENGTH_BYTES];
    timeout(read_timeout, recv.read_exact(&mut len_bytes))
        .await
        .map_err(|_| ZakuraHandlerError::Timeout("control length"))??;
    let len = (&len_bytes[..]).read_u32::<LittleEndian>()?;
    if len == 0 || len > max_bytes {
        return Err(ZakuraHandlerError::Oversize);
    }
    let mut bytes = vec![0; len as usize];
    timeout(read_timeout, recv.read_exact(&mut bytes))
        .await
        .map_err(|_| ZakuraHandlerError::Timeout("control payload"))??;
    Ok(bytes)
}

async fn write_control_payload(
    send: &mut SendStream,
    bytes: &[u8],
    write_timeout: Duration,
) -> Result<(), ZakuraHandlerError> {
    let mut len = Vec::with_capacity(CONTROL_LENGTH_BYTES);
    len.write_u32::<LittleEndian>(bytes.len() as u32)?;
    timeout(write_timeout, send.write_all(&len))
        .await
        .map_err(|_| ZakuraHandlerError::Timeout("control length write"))??;
    timeout(write_timeout, send.write_all(bytes))
        .await
        .map_err(|_| ZakuraHandlerError::Timeout("control payload write"))??;
    let _ = send.finish();
    Ok(())
}

async fn write_ordered_frame(
    send: &mut SendStream,
    frame: Frame,
    limits: ZakuraConnectionLimits,
    stream_kind: u16,
) -> Result<(), BoxError> {
    // Mirror `write_response_frame`: a persistent ordered-stream frame whose
    // payload exceeds the peer's negotiated `max_message_bytes` would be
    // rejected by the peer as oversize, so reject it locally before wasting
    // encode/write work rather than writing a frame the peer cannot accept. The
    // handshake clamps `max_frame_bytes` and `max_message_bytes` independently,
    // so the frame cap alone does not bound this.
    if frame.payload.len() > limits.max_message_bytes as usize {
        return Err(format!(
            "Zakura outbound ordered frame payload {} exceeds negotiated max_message_bytes {}",
            frame.payload.len(),
            limits.max_message_bytes,
        )
        .into());
    }
    let frame = frame.encode(app_frame_cap_for_stream_kind(&limits, stream_kind))?;
    timeout(OUTBOUND_STREAM_WRITE_TIMEOUT, send.write_all(&frame))
        .await
        .map_err(|_| -> BoxError { "Zakura outbound frame write timed out".into() })??;
    Ok(())
}

async fn write_outbound_request_frame(
    connection: &Connection,
    limits: ZakuraConnectionLimits,
    stream_kind: u16,
    request_id: u64,
    message_type: u16,
    flags: u16,
    payload: Vec<u8>,
) -> Result<Vec<Frame>, OutboundRequestError> {
    timeout(
        OUTBOUND_REQUEST_RESPONSE_TIMEOUT,
        write_outbound_request_frame_inner(
            connection,
            limits,
            stream_kind,
            request_id,
            message_type,
            flags,
            payload,
        ),
    )
    .await
    .map_err(|_| OutboundRequestError::Local("Zakura outbound request/response timed out".into()))?
}

async fn write_outbound_request_frame_inner(
    connection: &Connection,
    limits: ZakuraConnectionLimits,
    stream_kind: u16,
    request_id: u64,
    message_type: u16,
    flags: u16,
    payload: Vec<u8>,
) -> Result<Vec<Frame>, OutboundRequestError> {
    let budget = LegacyResponseBudget::from_request(message_type, &payload, limits)?;
    let (mut send, mut recv) = timeout(OUTBOUND_STREAM_WRITE_TIMEOUT, connection.open_bi())
        .await
        .map_err(|_| -> BoxError { "Zakura outbound request stream open timed out".into() })
        .map_err(OutboundRequestError::Local)?
        .map_err(|error| OutboundRequestError::Local(Box::new(error)))?;
    let prelude = StreamPrelude {
        magic: STREAM_PRELUDE_MAGIC,
        stream_kind,
        stream_version: ZAKURA_STREAM_VERSION_1,
        request_id: Some(request_id),
        max_frame_bytes: app_frame_cap_for_stream_kind(&limits, stream_kind),
    };
    let frame = Frame {
        message_type,
        flags,
        payload,
    };
    let prelude = prelude.encode().map_err(|error| {
        OutboundRequestError::Local(BoxError::from(format!("failed to encode prelude: {error}")))
    })?;
    let frame = frame
        .encode(app_frame_cap_for_stream_kind(&limits, stream_kind))
        .map_err(|error| OutboundRequestError::Local(Box::new(error)))?;
    timeout(OUTBOUND_STREAM_WRITE_TIMEOUT, send.write_all(&prelude))
        .await
        .map_err(|_| -> BoxError { "Zakura outbound request prelude write timed out".into() })
        .map_err(OutboundRequestError::Local)?
        .map_err(|error| OutboundRequestError::Local(Box::new(error)))?;
    timeout(OUTBOUND_STREAM_WRITE_TIMEOUT, send.write_all(&frame))
        .await
        .map_err(|_| -> BoxError { "Zakura outbound request frame write timed out".into() })
        .map_err(OutboundRequestError::Local)?
        .map_err(|error| OutboundRequestError::Local(Box::new(error)))?;
    let _ = send.finish();

    let mut frames = Vec::new();
    let mut state = LegacyResponseReadState::new(budget);
    loop {
        match read_frame(
            &mut recv,
            app_frame_cap_for_stream_kind(&limits, stream_kind),
            limits.idle_timeout,
        )
        .await
        {
            Ok(frame) => {
                state.validate_frame(request_id, &frame)?;
                frames.push(frame);
            }
            Err(ZakuraHandlerError::Closed) => {
                state.finish()?;
                return Ok(frames);
            }
            Err(ZakuraHandlerError::Timeout(_)) => {
                return Err(OutboundRequestError::Local(Box::new(
                    ZakuraHandlerError::Timeout("outbound response"),
                )));
            }
            Err(error @ ZakuraHandlerError::OversizeFrame { .. })
            | Err(error @ ZakuraHandlerError::Oversize) => {
                return Err(OutboundRequestError::Fatal(Box::new(error)));
            }
            Err(error) => return Err(OutboundRequestError::Fatal(Box::new(error))),
        }
    }
}

#[derive(Debug)]
enum OutboundRequestError {
    Local(BoxError),
    Fatal(BoxError),
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum LegacyResponseKind {
    Blocks,
    Transactions,
    BlockHashes,
    BlockHeaders,
    TransactionIds,
    Pong,
    Nil,
}

#[derive(Copy, Clone, Debug)]
struct LegacyResponseBudget {
    kind: LegacyResponseKind,
    max_items: usize,
    max_frames: usize,
    max_bytes: usize,
    max_message_bytes: usize,
}

impl LegacyResponseBudget {
    fn from_request(
        message_type: u16,
        payload: &[u8],
        limits: ZakuraConnectionLimits,
    ) -> Result<Self, OutboundRequestError> {
        let kind = match message_type {
            LEGACY_REQUEST_BLOCKS_BY_HASH => LegacyResponseKind::Blocks,
            LEGACY_REQUEST_TRANSACTIONS_BY_ID => LegacyResponseKind::Transactions,
            LEGACY_REQUEST_FIND_BLOCKS => LegacyResponseKind::BlockHashes,
            LEGACY_REQUEST_FIND_HEADERS => LegacyResponseKind::BlockHeaders,
            LEGACY_REQUEST_MEMPOOL_TRANSACTION_IDS => LegacyResponseKind::TransactionIds,
            LEGACY_REQUEST_PING => LegacyResponseKind::Pong,
            LEGACY_REQUEST_PUSH_TRANSACTION => LegacyResponseKind::Nil,
            _ => {
                return Err(OutboundRequestError::Local(
                    format!("unsupported legacy request message type: {message_type}").into(),
                ));
            }
        };
        let max_message_bytes = usize::try_from(limits.max_message_bytes)
            .map_err(|error| OutboundRequestError::Local(Box::new(error)))?;
        let max_inventory_items = usize::try_from(MAX_TX_INV_IN_SENT_MESSAGE)
            .map_err(|error| OutboundRequestError::Local(Box::new(error)))?;
        let (max_items, max_frames, max_bytes) = match kind {
            LegacyResponseKind::Blocks | LegacyResponseKind::Transactions => {
                let item_count = legacy_inventory_count(payload, kind)?;
                let max_frames = item_count
                    .saturating_mul(LEGACY_RESPONSE_MAX_FRAMES_PER_ITEM)
                    .saturating_add(1);
                let item_bytes = match kind {
                    LegacyResponseKind::Blocks => LEGACY_BLOCK_HASH_BYTES,
                    LegacyResponseKind::Transactions => LEGACY_INVENTORY_HASH_BYTES,
                    _ => unreachable!("matched inventory response kind"),
                };
                let missing_bytes = LEGACY_RESPONSE_REQUEST_ID_BYTES
                    .saturating_add(LEGACY_COMPACT_SIZE_PREFIX_BYTES)
                    .saturating_add(item_count.saturating_mul(item_bytes));
                let max_bytes = item_count
                    .saturating_mul(max_message_bytes)
                    .saturating_add(missing_bytes)
                    .max(LEGACY_RESPONSE_CHUNK_HEADER_BYTES);
                (item_count, max_frames, max_bytes)
            }
            LegacyResponseKind::BlockHashes => {
                let max_bytes = LEGACY_RESPONSE_REQUEST_ID_BYTES
                    .saturating_add(LEGACY_COMPACT_SIZE_PREFIX_BYTES)
                    .saturating_add(max_inventory_items.saturating_mul(LEGACY_BLOCK_HASH_BYTES));
                (max_inventory_items, 1, max_bytes)
            }
            LegacyResponseKind::BlockHeaders => (
                MAX_HEADERS_PER_MESSAGE,
                1,
                LEGACY_RESPONSE_REQUEST_ID_BYTES.saturating_add(max_message_bytes),
            ),
            LegacyResponseKind::TransactionIds => {
                let max_bytes = LEGACY_RESPONSE_REQUEST_ID_BYTES
                    .saturating_add(LEGACY_COMPACT_SIZE_PREFIX_BYTES)
                    .saturating_add(
                        max_inventory_items.saturating_mul(LEGACY_INVENTORY_HASH_BYTES),
                    );
                (max_inventory_items, 1, max_bytes)
            }
            LegacyResponseKind::Pong | LegacyResponseKind::Nil => {
                (1, 1, LEGACY_RESPONSE_REQUEST_ID_BYTES)
            }
        };

        // Clamp the retained-frame byte budget to a fixed operational aggregate
        // cap. For Blocks/Transactions the derived `max_bytes` scales with the
        // requested inventory count (`item_count * max_message_bytes`), so a
        // request naming the protocol-max inventory count would otherwise let a
        // hostile responder accumulate tens of GiB of validated frames in the
        // accepted-frame `Vec` before `decode_response` runs. The inbound
        // responder already bounds a single response to the same aggregate, so
        // this only rejects responses an honest peer would never send.
        let max_bytes = max_bytes.min(LEGACY_RESPONSE_MAX_AGGREGATE_BYTES);

        Ok(Self {
            kind,
            max_items,
            max_frames,
            max_bytes,
            max_message_bytes,
        })
    }
}

#[derive(Debug)]
struct LegacyResponseReadState {
    budget: LegacyResponseBudget,
    frames: usize,
    bytes: usize,
    items: usize,
    active_chunk_type: Option<u16>,
    active_chunk: Vec<u8>,
}

impl LegacyResponseReadState {
    fn new(budget: LegacyResponseBudget) -> Self {
        Self {
            budget,
            frames: 0,
            bytes: 0,
            items: 0,
            active_chunk_type: None,
            active_chunk: Vec::new(),
        }
    }

    fn validate_frame(
        &mut self,
        request_id: u64,
        frame: &Frame,
    ) -> Result<(), OutboundRequestError> {
        if frame.flags != 0 {
            return Err(OutboundRequestError::Fatal(
                format!("unsupported legacy response flags: {}", frame.flags).into(),
            ));
        }

        self.frames = self.frames.saturating_add(1);
        if self.frames > self.budget.max_frames {
            return Err(OutboundRequestError::Fatal(
                "too many legacy response frames".into(),
            ));
        }

        self.bytes = self.bytes.saturating_add(frame.payload.len());
        if self.bytes > self.budget.max_bytes {
            return Err(OutboundRequestError::Fatal(
                "legacy response exceeded cumulative byte budget".into(),
            ));
        }

        match frame.message_type {
            LEGACY_RESPONSE_BLOCK => {
                self.validate_available_chunk(request_id, frame, LegacyResponseKind::Blocks)
            }
            LEGACY_RESPONSE_TRANSACTION => {
                self.validate_available_chunk(request_id, frame, LegacyResponseKind::Transactions)
            }
            LEGACY_RESPONSE_MISSING_BLOCKS => {
                self.validate_missing(request_id, &frame.payload, LegacyResponseKind::Blocks)
            }
            LEGACY_RESPONSE_MISSING_TRANSACTIONS => {
                self.validate_missing(request_id, &frame.payload, LegacyResponseKind::Transactions)
            }
            LEGACY_RESPONSE_BLOCK_HASHES => self.validate_id_prefixed_hashes(
                request_id,
                &frame.payload,
                LegacyResponseKind::BlockHashes,
            ),
            LEGACY_RESPONSE_BLOCK_HEADERS => self.validate_headers(request_id, &frame.payload),
            LEGACY_RESPONSE_TRANSACTION_IDS => self.validate_id_prefixed_hashes(
                request_id,
                &frame.payload,
                LegacyResponseKind::TransactionIds,
            ),
            LEGACY_RESPONSE_PONG => {
                self.validate_id_only(request_id, &frame.payload, LegacyResponseKind::Pong)
            }
            LEGACY_RESPONSE_NIL => self.validate_nil(request_id, &frame.payload),
            message_type => Err(OutboundRequestError::Fatal(
                format!("unknown legacy response message type: {message_type}").into(),
            )),
        }
    }

    fn finish(self) -> Result<(), OutboundRequestError> {
        if self.active_chunk_type.is_some() {
            return Err(OutboundRequestError::Fatal(
                "incomplete legacy response chunk".into(),
            ));
        }
        Ok(())
    }

    fn validate_available_chunk(
        &mut self,
        request_id: u64,
        frame: &Frame,
        kind: LegacyResponseKind,
    ) -> Result<(), OutboundRequestError> {
        if self.budget.kind != kind {
            return Err(OutboundRequestError::Fatal(
                "legacy response kind does not match request".into(),
            ));
        }

        let (response_id, is_last, bytes) = legacy_response_chunk_header(&frame.payload)?;
        if response_id != request_id {
            return Err(OutboundRequestError::Fatal(
                format!(
                    "wrong legacy response request id: expected {request_id}, got {response_id}"
                )
                .into(),
            ));
        }

        match self.active_chunk_type {
            Some(active) if active != frame.message_type => {
                return Err(OutboundRequestError::Fatal(
                    "interleaved legacy response chunks".into(),
                ));
            }
            None => self.active_chunk_type = Some(frame.message_type),
            _ => {}
        }

        let new_len = self.active_chunk.len().saturating_add(bytes.len());
        if new_len > self.budget.max_message_bytes {
            return Err(OutboundRequestError::Fatal(
                "legacy response item exceeded message byte cap".into(),
            ));
        }
        self.active_chunk.extend_from_slice(bytes);

        if is_last {
            self.validate_completed_item(kind)?;
            self.active_chunk_type = None;
            self.active_chunk.clear();
            self.add_items(1)?;
        }

        Ok(())
    }

    fn validate_completed_item(
        &self,
        kind: LegacyResponseKind,
    ) -> Result<(), OutboundRequestError> {
        match kind {
            LegacyResponseKind::Blocks => {
                Block::zcash_deserialize(&mut Cursor::new(self.active_chunk.as_slice()))
                    .map(|_| ())
                    .map_err(|error| OutboundRequestError::Fatal(Box::new(error)))
            }
            LegacyResponseKind::Transactions => {
                Transaction::zcash_deserialize(&mut Cursor::new(self.active_chunk.as_slice()))
                    .map(|_| ())
                    .map_err(|error| OutboundRequestError::Fatal(Box::new(error)))
            }
            LegacyResponseKind::BlockHashes
            | LegacyResponseKind::BlockHeaders
            | LegacyResponseKind::TransactionIds
            | LegacyResponseKind::Pong
            | LegacyResponseKind::Nil => unreachable!("non-chunk legacy response kind"),
        }
    }

    fn validate_missing(
        &mut self,
        request_id: u64,
        payload: &[u8],
        kind: LegacyResponseKind,
    ) -> Result<(), OutboundRequestError> {
        let payload = self.begin_id_prefixed_response(kind, request_id, payload, "missing")?;
        let count = legacy_inventory_count(payload, kind)?;
        self.add_items(count)
    }

    fn validate_id_prefixed_hashes(
        &mut self,
        request_id: u64,
        payload: &[u8],
        kind: LegacyResponseKind,
    ) -> Result<(), OutboundRequestError> {
        let payload = self.begin_id_prefixed_response(kind, request_id, payload, "list")?;
        let count = legacy_inventory_count(payload, kind)?;
        self.add_items(count)
    }

    fn validate_headers(
        &mut self,
        request_id: u64,
        payload: &[u8],
    ) -> Result<(), OutboundRequestError> {
        let payload = self.begin_id_prefixed_response(
            LegacyResponseKind::BlockHeaders,
            request_id,
            payload,
            "headers",
        )?;
        let count = legacy_header_count(payload)?;
        self.add_items(count)
    }

    fn validate_id_only(
        &mut self,
        request_id: u64,
        payload: &[u8],
        kind: LegacyResponseKind,
    ) -> Result<(), OutboundRequestError> {
        let payload =
            self.begin_id_prefixed_response(kind, request_id, payload, "acknowledgement")?;
        if !payload.is_empty() {
            return Err(OutboundRequestError::Fatal(
                "legacy acknowledgement response has trailing bytes".into(),
            ));
        }
        self.add_items(1)
    }

    /// Validate a `MSG_RESPONSE_NIL` empty-result sentinel against the request kind.
    ///
    /// NIL is the empty-result sentinel only for chain-discovery and mempool
    /// queries: the inbound service answers an empty `FindBlocks`/`FindHeaders`/
    /// `MempoolTransactionIds` (and a queued `PushTransaction`) with
    /// `Response::Nil`. Inventory fetches (`BlocksByHash`/`TransactionsById`) and
    /// `Ping` must never receive a bare NIL, so reject it as `Fatal` for those
    /// kinds — fail closed and let the request stream worker disconnect the peer,
    /// matching `LegacyResponseCodec::decode_response`, which rejects NIL for the
    /// same kinds. The kind-specific empty `Response` is produced later by
    /// `decode_response`.
    fn validate_nil(
        &mut self,
        request_id: u64,
        payload: &[u8],
    ) -> Result<(), OutboundRequestError> {
        match self.budget.kind {
            LegacyResponseKind::BlockHashes
            | LegacyResponseKind::BlockHeaders
            | LegacyResponseKind::TransactionIds
            | LegacyResponseKind::Nil => {}
            LegacyResponseKind::Blocks
            | LegacyResponseKind::Transactions
            | LegacyResponseKind::Pong => {
                return Err(OutboundRequestError::Fatal(
                    "unexpected legacy nil response for inventory or ping request".into(),
                ));
            }
        }
        if self.active_chunk_type.is_some() {
            return Err(OutboundRequestError::Fatal(
                "legacy nil response interleaved with response chunk".into(),
            ));
        }
        let (response_id, payload) = legacy_response_id(payload)?;
        if response_id != request_id {
            return Err(OutboundRequestError::Fatal(
                format!(
                    "wrong legacy nil response request id: expected {request_id}, got {response_id}"
                )
                .into(),
            ));
        }
        if !payload.is_empty() {
            return Err(OutboundRequestError::Fatal(
                "legacy nil response has trailing bytes".into(),
            ));
        }
        self.add_items(1)
    }

    fn begin_id_prefixed_response<'a>(
        &self,
        kind: LegacyResponseKind,
        request_id: u64,
        payload: &'a [u8],
        label: &'static str,
    ) -> Result<&'a [u8], OutboundRequestError> {
        if self.budget.kind != kind {
            return Err(OutboundRequestError::Fatal(
                format!("legacy {label} response kind does not match request").into(),
            ));
        }
        if self.active_chunk_type.is_some() {
            return Err(OutboundRequestError::Fatal(
                format!("legacy {label} response interleaved with response chunk").into(),
            ));
        }
        let (response_id, payload) = legacy_response_id(payload)?;
        if response_id != request_id {
            return Err(OutboundRequestError::Fatal(
                format!(
                    "wrong legacy {label} response request id: expected {request_id}, got {response_id}"
                )
                    .into(),
            ));
        }
        Ok(payload)
    }

    fn add_items(&mut self, count: usize) -> Result<(), OutboundRequestError> {
        self.items = self.items.saturating_add(count);
        if self.items > self.budget.max_items {
            return Err(OutboundRequestError::Fatal(
                "legacy response contained more items than requested".into(),
            ));
        }
        Ok(())
    }
}

fn legacy_inventory_count(
    payload: &[u8],
    kind: LegacyResponseKind,
) -> Result<usize, OutboundRequestError> {
    let mut reader = Cursor::new(payload);
    let count = usize::from(
        CompactSizeMessage::zcash_deserialize(&mut reader)
            .map_err(|error| OutboundRequestError::Fatal(Box::new(error)))?,
    );

    match kind {
        LegacyResponseKind::Blocks | LegacyResponseKind::BlockHashes => {
            let consumed = usize::try_from(reader.position())
                .map_err(|error| OutboundRequestError::Fatal(Box::new(error)))?;
            let expected_len =
                consumed.saturating_add(count.saturating_mul(LEGACY_BLOCK_HASH_BYTES));
            if expected_len != payload.len() {
                return Err(OutboundRequestError::Fatal(
                    "legacy block inventory has trailing or truncated bytes".into(),
                ));
            }
        }
        LegacyResponseKind::Transactions | LegacyResponseKind::TransactionIds => {
            for _ in 0..count {
                let inventory = InventoryHash::zcash_deserialize(&mut reader)
                    .map_err(|error| OutboundRequestError::Fatal(Box::new(error)))?;
                if inventory.unmined_tx_id().is_none() {
                    return Err(OutboundRequestError::Fatal(
                        "legacy transaction inventory contained a non-transaction item".into(),
                    ));
                }
            }
            reject_legacy_inventory_trailing(payload, &reader)?;
        }
        LegacyResponseKind::BlockHeaders | LegacyResponseKind::Pong | LegacyResponseKind::Nil => {
            return Err(OutboundRequestError::Local(
                "legacy response kind does not use inventory counts".into(),
            ));
        }
    }

    Ok(count)
}

fn legacy_header_count(payload: &[u8]) -> Result<usize, OutboundRequestError> {
    let mut reader = Cursor::new(payload);
    let count = usize::from(
        CompactSizeMessage::zcash_deserialize(&mut reader)
            .map_err(|error| OutboundRequestError::Fatal(Box::new(error)))?,
    );
    if count > MAX_HEADERS_PER_MESSAGE {
        return Err(OutboundRequestError::Fatal(
            "legacy headers response exceeded header count cap".into(),
        ));
    }
    for _ in 0..count {
        CountedHeader::zcash_deserialize(&mut reader)
            .map_err(|error| OutboundRequestError::Fatal(Box::new(error)))?;
    }
    reject_legacy_inventory_trailing(payload, &reader)?;
    Ok(count)
}

fn reject_legacy_inventory_trailing(
    payload: &[u8],
    reader: &Cursor<&[u8]>,
) -> Result<(), OutboundRequestError> {
    if usize::try_from(reader.position())
        .map_err(|error| OutboundRequestError::Fatal(Box::new(error)))?
        != payload.len()
    {
        return Err(OutboundRequestError::Fatal(
            "legacy transaction inventory has trailing bytes".into(),
        ));
    }
    Ok(())
}

fn legacy_response_id(payload: &[u8]) -> Result<(u64, &[u8]), OutboundRequestError> {
    if payload.len() < LEGACY_RESPONSE_REQUEST_ID_BYTES {
        return Err(OutboundRequestError::Fatal(
            "truncated legacy response id".into(),
        ));
    }
    let mut id = [0; LEGACY_RESPONSE_REQUEST_ID_BYTES];
    id.copy_from_slice(&payload[..LEGACY_RESPONSE_REQUEST_ID_BYTES]);
    Ok((
        u64::from_le_bytes(id),
        &payload[LEGACY_RESPONSE_REQUEST_ID_BYTES..],
    ))
}

fn legacy_response_chunk_header(
    payload: &[u8],
) -> Result<(u64, bool, &[u8]), OutboundRequestError> {
    if payload.len() < LEGACY_RESPONSE_CHUNK_HEADER_BYTES {
        return Err(OutboundRequestError::Fatal(
            "truncated legacy response chunk".into(),
        ));
    }
    let (request_id, payload) = legacy_response_id(payload)?;
    Ok((request_id, payload[0] != 0, &payload[1..]))
}

async fn write_response_frame(
    send: &mut SendStream,
    frame: Frame,
    limits: ZakuraConnectionLimits,
) -> Result<(), ZakuraHandlerError> {
    if frame.payload.len() > limits.max_message_bytes as usize {
        return Err(ZakuraHandlerError::Oversize);
    }
    let frame = frame.encode(limits.max_frame_bytes)?;
    timeout(OUTBOUND_STREAM_WRITE_TIMEOUT, send.write_all(&frame))
        .await
        .map_err(|_| ZakuraHandlerError::Timeout("frame write"))??;
    Ok(())
}

fn validate_idle_invariant(limits: &ZakuraLocalLimits) -> Result<(), ZakuraHandlerError> {
    if limits.keep_alive_interval >= limits.quic_idle_timeout {
        return Err(ZakuraHandlerError::InvalidLocalLimits);
    }
    if limits.initial_limits().idle_timeout_millis as u128 >= limits.quic_idle_timeout.as_millis() {
        return Err(ZakuraHandlerError::InvalidLocalLimits);
    }
    Ok(())
}

fn zakura_secret_key(config: &Config) -> Result<SecretKey, ZakuraHandlerError> {
    // Loads the configured key, or loads/generates+persists a stable key under the
    // cache dir so the node keeps a consistent NodeId across restarts.
    config
        .zakura_secret_key()
        .map_err(|_| ZakuraHandlerError::InvalidSecretKey)
}

fn stream_kind_label(stream_kind: u16) -> &'static str {
    match stream_kind {
        0 => "control",
        1 => "request",
        LEGACY_GOSSIP_STREAM_KIND => "gossip",
        LEGACY_REQUEST_STREAM_KIND => "legacy_request",
        DISCOVERY_STREAM_KIND => "discovery",
        HEADER_SYNC_STREAM_KIND => "header_sync",
        ZAKURA_STREAM_BLOCK_SYNC => "block_sync",
        _ => "unknown",
    }
}

fn app_frame_cap_for_stream_kind(limits: &ZakuraConnectionLimits, stream_kind: u16) -> u32 {
    match stream_kind {
        HEADER_SYNC_STREAM_KIND => {
            let header_sync_cap =
                u32::try_from(MAX_HS_MESSAGE_BYTES.saturating_add(FRAME_HEADER_BYTES))
                    .expect("header-sync frame cap fits in u32");
            limits.max_frame_bytes.min(header_sync_cap)
        }
        ZAKURA_STREAM_BLOCK_SYNC => limits.max_frame_bytes.min(MAX_BS_FRAME_BYTES),
        _ => limits.max_frame_bytes.min(LOCAL_MAX_CONTROL_FRAME_BYTES),
    }
    .max(1)
}

/// Frame cap for reading on an admitted inbound stream, never larger than the
/// message cap allows.
///
/// On an admitted ordered/request stream a frame payload *is* the message, so
/// `admit_inbound_message` rejects any payload over `max_message_bytes`. A peer
/// can negotiate `max_frame_bytes > max_message_bytes` (the two caps are clamped
/// independently in `ZakuraLocalLimits::clamp`), so the cap handed to
/// `read_frame` must also be limited to the message size. Otherwise a frame whose
/// `payload_len` falls between the two limits is allocated and read in full by
/// `read_frame` before `admit_inbound_message` rejects it as oversize, letting a
/// peer force per-frame allocation/I/O up to the larger frame cap across many
/// streams.
fn inbound_frame_cap_for_stream_kind(limits: &ZakuraConnectionLimits, stream_kind: u16) -> u32 {
    let frame_header_bytes =
        u32::try_from(FRAME_HEADER_BYTES).expect("frame header byte count fits in u32");
    app_frame_cap_for_stream_kind(limits, stream_kind)
        .min(limits.max_message_bytes.saturating_add(frame_header_bytes))
}

fn per_stream_inbound_queue_depth(
    max_inbound_queue_depth: u16,
    ordered_stream_count: usize,
) -> usize {
    let total = usize::from(max_inbound_queue_depth).max(1);
    if ordered_stream_count == 0 {
        return total;
    }

    total.saturating_div(ordered_stream_count).max(1)
}

fn should_run_freshness_reaper(
    ordered_stream_count: usize,
    request_response_stream_count: usize,
) -> bool {
    ordered_stream_count > 0 || request_response_stream_count == 0
}

/// The only stream-kind version this v1 handler serves. Every known kind is
/// at version 1; a peer naming any other version of a known kind is rejected.
const ZAKURA_STREAM_VERSION_1: u16 = 1;
const ZAKURA_STREAM_VERSION_2: u16 = 2;

/// Returns whether the handler can serve a stream with this kind and version.
///
/// A peer controls both fields of the prelude, so an unknown kind or an
/// unsupported version of a known kind must be rejected before the stream
/// consumes a worker, a stream permit, queue depth, or rate budget. Keeping
/// this in one place means [`stream_kind_label`] (used for metrics/trace) and
/// admission agree on what "known" means.
#[cfg(test)]
fn is_supported_stream(registry: &ServiceRegistry, stream_kind: u16, stream_version: u16) -> bool {
    registry
        .capability_for_stream(stream_kind, stream_version)
        .is_some()
}

/// One message-rate [`TokenBucket`] shared by every stream worker serving the
/// same stream kind on one connection.
///
/// A worker holds this briefly (no `.await` while locked) to spend one token
/// per decoded frame, so N concurrent same-kind streams draw from a single
/// per-connection budget instead of N independent ones (FLUP-014).
type SharedMessageBucket<C = RealClock> = Arc<std::sync::Mutex<TokenBucket<C>>>;

/// Per-connection collection of message-rate buckets keyed by validated stream
/// kind. Created lazily (FLUP-015 rejects unknown kinds before this point, so
/// every key here is a known kind) and shared across that connection's workers.
type MessageRateBuckets<C = RealClock> = HashMap<u16, SharedMessageBucket<C>>;

/// Returns the shared message-rate bucket for `stream_kind` on this connection,
/// creating it sized from `message_rate_per_second` on first use.
///
/// Two workers serving the same kind get [`Arc::clone`]s of the same bucket, so
/// their `try_take` calls draw from one budget (FLUP-014). Generic over the
/// clock so tests can drive deterministic refills with a `TestClock`.
fn message_bucket_for<C: Clock>(
    buckets: &mut MessageRateBuckets<C>,
    stream_kind: u16,
    message_rate_per_second: u32,
    clock: C,
) -> SharedMessageBucket<C> {
    buckets
        .entry(stream_kind)
        .or_insert_with(|| {
            Arc::new(std::sync::Mutex::new(TokenBucket::with_clock(
                message_rate_per_second,
                clock,
            )))
        })
        .clone()
}

#[derive(Clone, Debug)]
struct TokenBucket<C = RealClock> {
    capacity: u32,
    tokens: u32,
    refill_per_second: u32,
    last_refill: Instant,
    clock: C,
}

impl TokenBucket<RealClock> {
    fn new(refill_per_second: u32) -> Self {
        Self::with_clock(refill_per_second, RealClock)
    }
}

impl<C: Clock> TokenBucket<C> {
    fn with_clock(refill_per_second: u32, clock: C) -> Self {
        let capacity = refill_per_second.max(1);
        Self {
            capacity,
            tokens: capacity,
            refill_per_second: capacity,
            last_refill: clock.now(),
            clock,
        }
    }

    fn try_take(&mut self) -> bool {
        self.refill(self.clock.now());
        if self.tokens == 0 {
            return false;
        }
        self.tokens -= 1;
        true
    }

    fn refill(&mut self, now: Instant) {
        let elapsed = now.saturating_duration_since(self.last_refill);
        let elapsed_nanos = elapsed.as_nanos();
        let new_tokens =
            elapsed_nanos.saturating_mul(u128::from(self.refill_per_second)) / 1_000_000_000;
        if new_tokens == 0 {
            return;
        }
        let new_tokens = new_tokens.min(u128::from(u32::MAX)) as u32;
        self.tokens = self.capacity.min(self.tokens.saturating_add(new_tokens));
        self.last_refill = now;
    }
}

/// Errors produced by the Zakura protocol handler.
#[derive(Debug, Error)]
pub enum ZakuraHandlerError {
    /// A bounded read/write timed out.
    #[error("Zakura {0} timed out")]
    Timeout(&'static str),
    /// A peer-controlled payload exceeded its cap.
    #[error("Zakura payload exceeded its cap")]
    Oversize,
    /// A peer declared a frame larger than the effective receiver cap.
    #[error(
        "Zakura frame length {frame_len} exceeded cap {max_frame_bytes} \
         (payload length {payload_len})"
    )]
    OversizeFrame {
        /// Declared frame payload length.
        payload_len: usize,
        /// Full frame length including the fixed header.
        frame_len: usize,
        /// Effective frame cap used before reading the payload.
        max_frame_bytes: usize,
    },
    /// The peer closed the stream or connection.
    #[error("Zakura stream closed")]
    Closed,
    /// The configured bootstrap peer is malformed.
    #[error("invalid Zakura bootstrap peer")]
    InvalidBootstrapPeer,
    /// The configured iroh secret key is malformed.
    #[error("invalid Zakura iroh secret key")]
    InvalidSecretKey,
    /// Local Zakura limits violate an invariant.
    #[error("invalid Zakura local limits")]
    InvalidLocalLimits,
    /// A local resource cap rejected the operation.
    #[error("Zakura resource limit exceeded: {0}")]
    ResourceLimit(&'static str),
    /// The peer exceeded its per-kind inbound message rate.
    #[error("Zakura message rate exceeded")]
    RateLimited,
    /// Iroh connection error.
    #[error(transparent)]
    IrohConnection(#[from] iroh::endpoint::ConnectionError),
    /// Iroh connect error.
    #[error(transparent)]
    IrohConnect(#[from] iroh::endpoint::ConnectError),
    /// Iroh remote id error.
    #[error(transparent)]
    IrohRemoteId(#[from] iroh::endpoint::RemoteNodeIdError),
    /// Iroh write error.
    #[error(transparent)]
    IrohWrite(#[from] iroh::endpoint::WriteError),
    /// Iroh read error.
    #[error(transparent)]
    IrohRead(#[from] iroh::endpoint::ReadExactError),
    /// Closed stream.
    #[error(transparent)]
    IrohClosedStream(#[from] iroh::endpoint::ClosedStream),
    /// Zakura wire format error.
    #[error(transparent)]
    Protocol(#[from] ZakuraProtocolError),
    /// Zakura validation error.
    #[error(transparent)]
    Validation(#[from] super::ZakuraValidationError),
    /// I/O error while encoding or decoding local buffers.
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

impl ZakuraHandlerError {
    fn oversize_frame_details(&self) -> Option<(u64, u64, u64)> {
        let Self::OversizeFrame {
            payload_len,
            frame_len,
            max_frame_bytes,
        } = self
        else {
            return None;
        };

        Some((
            u64::try_from(*payload_len).unwrap_or(u64::MAX),
            u64::try_from(*frame_len).unwrap_or(u64::MAX),
            u64::try_from(*max_frame_bytes).unwrap_or(u64::MAX),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        protocol::internal::{InventoryResponse, Response},
        zakura::{
            legacy_gossip::{LegacyRequestFrame, LegacyRequestKind, LegacyResponseCodec},
            testkit::LocalEndpointFactory,
            HeaderSyncEvent, HeaderSyncMessage, HeaderSyncMisbehavior, HeaderSyncPeerSession,
            HeaderSyncStatus, ServicePeerLimits, ZakuraDiscoveryConfig, ZakuraDiscoveryHandle,
            ZakuraDiscoveryLocalConfig, LOCAL_MAX_MESSAGE_BYTES, MAX_HS_MESSAGE_BYTES,
            MSG_HS_STATUS, ZAKURA_CAP_DISCOVERY, ZAKURA_CAP_HEADER_SYNC, ZAKURA_CAP_LEGACY_GOSSIP,
        },
    };
    use iroh::{
        endpoint::Connection,
        protocol::{AcceptError, ProtocolHandler},
    };
    use zebra_chain::{
        block::{self, Block},
        serialization::{ZcashDeserialize, MAX_PROTOCOL_MESSAGE_LEN},
        transaction::{self, UnminedTxId},
    };
    use zebra_test::vectors::BLOCK_TESTNET_141042_BYTES;

    /// With no configured `zakura.listen_addr`, the native endpoint must bind
    /// loopback-only. Otherwise iroh's default bind (`0.0.0.0:0` / `[::]:0`)
    /// exposes the experimental P2P_V2_ALPN handshake/session surface on every
    /// interface on an OS-assigned ephemeral port, even though the unset state is
    /// documented as dial-out only.
    #[tokio::test]
    async fn unset_listen_addr_binds_loopback_not_unspecified() {
        let builder = direct_endpoint_builder(SecretKey::generate(OsRng));
        let builder = bind_native_endpoint(builder, None);
        let endpoint = builder.bind().await.expect("loopback bind should succeed");

        let sockets = endpoint.bound_sockets();
        assert!(
            !sockets.is_empty(),
            "endpoint should bind at least one socket"
        );
        for socket in &sockets {
            assert!(
                socket.ip().is_loopback(),
                "unset listen_addr must bind loopback only, but bound {socket} \
                 (exposes P2P_V2_ALPN on all interfaces)"
            );
        }

        endpoint.close().await;
    }

    #[derive(Debug, Clone)]
    struct CaptureConnection {
        connection_tx: mpsc::Sender<Connection>,
        stream_tx: mpsc::Sender<(SendStream, RecvStream)>,
    }

    impl ProtocolHandler for CaptureConnection {
        async fn accept(&self, connection: Connection) -> Result<(), AcceptError> {
            let _ = self.connection_tx.send(connection.clone()).await;
            for _ in 0..2 {
                let Ok(streams) = connection.accept_bi().await else {
                    break;
                };
                let _ = self.stream_tx.send(streams).await;
            }
            Ok(())
        }
    }

    #[derive(Debug, Default)]
    struct RecordingService {
        deliveries: std::sync::Mutex<Vec<(ZakuraPeerId, u16, u16)>>,
    }

    impl RecordingService {
        fn deliveries(&self) -> Vec<(ZakuraPeerId, u16, u16)> {
            self.deliveries
                .lock()
                .expect("recording sink mutex is never poisoned")
                .clone()
        }
    }

    impl Service for RecordingService {
        fn name(&self) -> &'static str {
            "recording"
        }

        fn streams(&self) -> &[Stream] {
            crate::zakura::legacy_gossip::legacy_gossip_streams()
        }

        fn add_peer(&self, _peer: Peer) {}

        fn remove_peer(&self, _peer: &ZakuraPeerId) {}

        fn deliver_frame(
            &self,
            peer_id: ZakuraPeerId,
            stream_kind: u16,
            frame: Frame,
        ) -> Result<(), SinkReject> {
            self.deliveries
                .lock()
                .map_err(|error| SinkReject::local(format!("recording sink poisoned: {error}")))?
                .push((peer_id, stream_kind, frame.message_type));
            Ok(())
        }
    }

    #[derive(Debug)]
    struct NoopService;

    impl Service for NoopService {
        fn name(&self) -> &'static str {
            "noop"
        }

        fn streams(&self) -> &[Stream] {
            &[]
        }

        fn add_peer(&self, _peer: Peer) {}

        fn remove_peer(&self, _peer: &ZakuraPeerId) {}
    }

    #[derive(Debug)]
    struct DeclaredStreamService {
        streams: Vec<Stream>,
    }

    impl Service for DeclaredStreamService {
        fn name(&self) -> &'static str {
            "declared-stream"
        }

        fn streams(&self) -> &[Stream] {
            &self.streams
        }

        fn add_peer(&self, _peer: Peer) {}

        fn remove_peer(&self, _peer: &ZakuraPeerId) {}
    }

    fn test_peer(byte: u8) -> ZakuraPeerId {
        ZakuraPeerId::new(vec![byte; 32]).expect("32-byte node id is valid")
    }

    #[test]
    fn native_duplicate_tie_breaker_converges_for_simultaneous_open() {
        let node_a = LocalEndpointFactory::secret_key(1).public();
        let node_b = LocalEndpointFactory::secret_key(2).public();
        let a_outbound =
            native_connection_transcript_hash(ServicePeerDirection::Outbound, &node_a, &node_b);
        let a_inbound =
            native_connection_transcript_hash(ServicePeerDirection::Inbound, &node_a, &node_b);
        let b_outbound =
            native_connection_transcript_hash(ServicePeerDirection::Outbound, &node_b, &node_a);
        let b_inbound =
            native_connection_transcript_hash(ServicePeerDirection::Inbound, &node_b, &node_a);

        assert_eq!(a_outbound, b_inbound);
        assert_eq!(a_inbound, b_outbound);
        assert_ne!(a_outbound, a_inbound);

        let winning_key = a_outbound.min(a_inbound);
        let losing_key = a_outbound.max(a_inbound);
        let peer = test_peer(7);
        let mut supervisor_a = ZakuraPeerSupervisor::default();
        let mut supervisor_b = ZakuraPeerSupervisor::default();
        assert!(matches!(
            supervisor_a.register_authenticated(peer.clone(), a_outbound),
            ZakuraUpgradeOutcome::Upgraded { .. }
        ));
        let _ = supervisor_a.register_authenticated(peer.clone(), a_inbound);
        assert!(matches!(
            supervisor_b.register_authenticated(peer.clone(), b_inbound),
            ZakuraUpgradeOutcome::Upgraded { .. }
        ));
        let _ = supervisor_b.register_authenticated(peer.clone(), b_outbound);

        assert!(matches!(
            supervisor_a.register_authenticated(peer.clone(), winning_key),
            ZakuraUpgradeOutcome::Duplicate { .. }
        ));
        assert!(matches!(
            supervisor_b.register_authenticated(peer.clone(), winning_key),
            ZakuraUpgradeOutcome::Duplicate { .. }
        ));
        assert!(matches!(
            supervisor_a.register_authenticated(peer.clone(), losing_key),
            ZakuraUpgradeOutcome::Duplicate { .. }
        ));
        assert!(matches!(
            supervisor_b.register_authenticated(peer, losing_key),
            ZakuraUpgradeOutcome::Duplicate { .. }
        ));
    }

    fn header_sync_test_session(
        peer: ZakuraPeerId,
    ) -> (HeaderSyncPeerSession, crate::zakura::FramedRecv) {
        let (send, recv) = crate::zakura::framed_channel(32);
        (
            HeaderSyncPeerSession::from_parts(peer, send, CancellationToken::new()),
            recv,
        )
    }

    fn test_discovery_service(supervisor: &ZakuraSupervisorHandle) -> Arc<dyn Service> {
        let (_handle, service) =
            test_discovery_service_with_peer_limits(supervisor, ServicePeerLimits::default());
        service
    }

    fn test_discovery_service_with_peer_limits(
        supervisor: &ZakuraSupervisorHandle,
        peer_limits: ServicePeerLimits,
    ) -> (ZakuraDiscoveryHandle, Arc<dyn Service>) {
        let handshake = ZakuraHandshakeConfig::for_network(&Network::Mainnet);
        let handle = ZakuraDiscoveryHandle::new(
            ZakuraDiscoveryLocalConfig {
                secret_key: SecretKey::from_bytes(&[7u8; 32]),
                direct_addrs: Vec::new(),
                services: crate::zakura::discovery::default_advertised_services(),
                zakura_protocol_min: handshake.zakura_protocol_min,
                zakura_protocol_max: handshake.zakura_protocol_max,
                network_id: handshake.network_id,
                chain_id: handshake.chain_id,
                last_authored_sequence: None,
            },
            ZakuraDiscoveryConfig {
                peer_limits,
                ..ZakuraDiscoveryConfig::default()
            },
            supervisor.subscribe(),
        )
        .expect("test discovery handle builds");
        let service =
            Arc::new(crate::zakura::DiscoveryService::new(handle.clone())) as Arc<dyn Service>;
        (handle, service)
    }

    fn header_sync_startup(shutdown: CancellationToken) -> HeaderSyncStartup {
        let network = Network::Mainnet;
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
        startup.shutdown = shutdown;
        startup
    }

    async fn next_header_sync_action(
        actions: &mut mpsc::Receiver<HeaderSyncAction>,
    ) -> HeaderSyncAction {
        tokio::time::timeout(Duration::from_secs(2), actions.recv())
            .await
            .expect("header-sync action arrives before timeout")
            .expect("header-sync action channel stays open")
    }

    fn status_at_genesis(network: &Network) -> HeaderSyncStatus {
        HeaderSyncStatus {
            tip_height: block::Height(0),
            tip_hash: network.genesis_hash(),
            anchor_height: block::Height(0),
            ..HeaderSyncStatus::default()
        }
    }

    async fn register_test_peer(
        supervisor: &ZakuraSupervisorHandle,
        peer: ZakuraPeerId,
        disconnect_token: CancellationToken,
    ) {
        let (outbound_tx, _outbound_rx) = mpsc::channel(1);
        let outbound_handle = ZakuraPeerHandle::new_for_tests(peer.clone(), outbound_tx);
        let registration = supervisor
            .register(
                peer.clone(),
                None,
                [peer.as_bytes()[0]; TRANSCRIPT_HASH_BYTES],
                outbound_handle,
                disconnect_token,
                ZAKURA_CAP_LEGACY_GOSSIP | ZAKURA_CAP_HEADER_SYNC,
            )
            .await;

        assert!(
            matches!(registration, ZakuraRegistration::Registered { .. }),
            "test peer should register once"
        );
    }

    #[test]
    fn local_limits_clamp_negotiated_values_down() {
        let config = Config::default();
        let limits = ZakuraLocalLimits::from_config(&config);
        let negotiated = ZakuraAcceptedLimits {
            max_frame_bytes: u32::MAX,
            max_message_bytes: u32::MAX,
            max_open_streams: u16::MAX,
            max_inbound_queue_depth: u16::MAX,
            idle_timeout_millis: u32::MAX,
        };

        let clamped = limits.clamp(&negotiated);

        assert_eq!(clamped.max_frame_bytes, limits.max_frame_bytes);
        assert_eq!(clamped.max_message_bytes, limits.max_message_bytes);
        assert_eq!(clamped.max_open_streams, limits.max_open_streams);
        assert_eq!(
            clamped.max_inbound_queue_depth,
            limits.max_inbound_queue_depth
        );
        assert!(clamped.idle_timeout < limits.quic_idle_timeout);
    }

    #[test]
    fn inbound_queue_depth_is_split_across_ordered_streams() {
        assert_eq!(per_stream_inbound_queue_depth(64, 2), 32);
        assert_eq!(per_stream_inbound_queue_depth(63, 2), 31);
        assert_eq!(per_stream_inbound_queue_depth(64, 0), 64);
        assert!(per_stream_inbound_queue_depth(63, 2) * 2 <= 63);
    }

    #[test]
    fn request_response_only_peers_do_not_use_ordered_stream_freshness_reaper() {
        assert!(!should_run_freshness_reaper(0, 1));
        assert!(should_run_freshness_reaper(1, 1));
        assert!(should_run_freshness_reaper(1, 0));
        assert!(should_run_freshness_reaper(0, 0));
    }

    #[tokio::test]
    async fn v2_p2p_false_leaves_header_sync_disabled() -> Result<(), BoxError> {
        let _guard = zebra_test::init();
        let config = Config {
            v2_p2p: false,
            ..Config::default()
        };

        let endpoint = spawn_zakura_endpoint(&config, |_supervisor, _trace| {
            Arc::new(NoopService) as Arc<dyn Service>
        })
        .await?;

        assert!(endpoint.is_none());
        Ok(())
    }

    #[tokio::test]
    async fn v2_p2p_true_starts_header_sync_handle() -> Result<(), BoxError> {
        let _guard = zebra_test::init();
        let config = Config::default();

        let endpoint = spawn_zakura_endpoint(&config, |_supervisor, _trace| {
            Arc::new(NoopService) as Arc<dyn Service>
        })
        .await?
        .expect("v2_p2p is enabled by default");

        assert!(endpoint.header_sync().is_some());
        endpoint.shutdown().await;
        Ok(())
    }

    #[tokio::test]
    async fn endpoint_shutdown_stops_header_sync_task() -> Result<(), BoxError> {
        let _guard = zebra_test::init();
        let endpoint = spawn_zakura_endpoint(&Config::default(), |_supervisor, _trace| {
            Arc::new(NoopService) as Arc<dyn Service>
        })
        .await?
        .expect("v2_p2p is enabled by default");
        let header_sync = endpoint
            .header_sync()
            .expect("header sync handle exists for a v2 endpoint");

        endpoint.shutdown().await;

        let (session, _recv) = header_sync_test_session(
            ZakuraPeerId::new(vec![5u8; 32]).expect("32-byte node id is valid"),
        );
        let send_result = tokio::time::timeout(
            Duration::from_secs(1),
            header_sync.send(HeaderSyncEvent::PeerConnected(session)),
        )
        .await
        .expect("send returns promptly after header-sync shutdown");
        assert!(send_result.is_err());

        Ok(())
    }

    /// A maintained native dial loop (shared by configured bootstrap peers, the
    /// legacy->Zakura upgrade hand-off, and `spawn_native_dial`) targets an
    /// unreachable peer, so its `maintain` policy retries forever and never
    /// exits on its own. `ZakuraEndpoint::shutdown` must still stop it: the task
    /// holds an endpoint clone, so the supervisor registration watch stays open
    /// and only the endpoint shutdown token can end the loop. Without that
    /// signal the detached loop outlives shutdown and keeps dialing.
    #[tokio::test]
    async fn endpoint_shutdown_stops_maintained_native_dial_loop() -> Result<(), BoxError> {
        let _guard = zebra_test::init();
        let endpoint = spawn_zakura_endpoint(&Config::default(), |_supervisor, _trace| {
            Arc::new(NoopService) as Arc<dyn Service>
        })
        .await?
        .expect("v2_p2p is enabled by default");

        // 192.0.2.0/24 is TEST-NET-1 (RFC 5737): guaranteed unreachable, so the
        // maintained loop stays in connect/backoff and never finishes on its own.
        let unreachable_addr: SocketAddr = "192.0.2.1:65535".parse().expect("valid test address");
        let unreachable = NodeAddr::new(LocalEndpointFactory::secret_key(987_654).public())
            .with_direct_addresses([unreachable_addr]);
        let dial = endpoint.spawn_native_dial(unreachable);

        // Let the maintained loop start before tearing the endpoint down.
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(
            !dial.is_finished(),
            "maintained dial loop to an unreachable peer should still be running"
        );

        endpoint.shutdown().await;

        tokio::time::timeout(Duration::from_secs(5), dial)
            .await
            .expect("maintained native dial loop must exit after endpoint shutdown")
            .expect("native dial task must not panic");
        Ok(())
    }

    /// The discovery candidate dialer is a long-lived loop that holds an
    /// endpoint clone, so its supervisor registration watch never closes on its
    /// own. `ZakuraEndpoint::shutdown` must still stop it via the endpoint
    /// shutdown token; otherwise it outlives teardown, polling the discovery
    /// book and attempting dials against a torn-down router.
    #[tokio::test]
    async fn endpoint_shutdown_stops_discovery_candidate_dialer() -> Result<(), BoxError> {
        let _guard = zebra_test::init();
        let config = Config::default();
        let endpoint = spawn_zakura_endpoint(&config, |_supervisor, _trace| {
            Arc::new(NoopService) as Arc<dyn Service>
        })
        .await?
        .expect("v2_p2p is enabled by default");

        let limits = ZakuraLocalLimits::from_config(&config);
        let handshake = ZakuraHandshakeConfig::for_network(&config.network);
        let discovery = crate::zakura::discovery::build_discovery_handle(
            SecretKey::generate(OsRng),
            Vec::new(),
            crate::zakura::discovery::default_advertised_services(),
            &handshake,
            limits.max_connections,
            0,
            endpoint.supervisor().subscribe(),
        )?;
        let dialer = crate::zakura::discovery::spawn_native_discovery_dialer(
            endpoint.clone(),
            discovery,
            limits,
        );

        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(
            !dialer.is_finished(),
            "discovery candidate dialer should still be running"
        );

        endpoint.shutdown().await;

        tokio::time::timeout(Duration::from_secs(5), dialer)
            .await
            .expect("discovery candidate dialer must exit after endpoint shutdown")
            .expect("discovery dialer task must not panic");
        Ok(())
    }

    /// A malicious legacy responder can return a syntactically valid
    /// `P2pV2UpgradeAccept` (a unique, real iroh node id with a parseable but
    /// unreachable direct address) that never completes native Zakura
    /// registration. After the upgrade hand-off wait times out, no
    /// maintain-forever native dial task or `upgrade_dials` entry may survive;
    /// otherwise repeating the failed upgrade with distinct node ids grows
    /// maintained dials and outbound QUIC traffic without bound.
    ///
    /// Regression test for `claude-legacy-upgrade-maintained-dial-leak`. Uses a
    /// paused clock so the 15s appear-timeout elapses without real waiting; the
    /// dial target is RFC 5737 TEST-NET-1, which never connects.
    #[tokio::test(start_paused = true)]
    async fn failed_legacy_upgrade_does_not_leak_maintained_dial() -> Result<(), BoxError> {
        let _guard = zebra_test::init();
        let endpoint = spawn_zakura_endpoint(&Config::default(), |_supervisor, _trace| {
            Arc::new(NoopService) as Arc<dyn Service>
        })
        .await?
        .expect("v2_p2p is enabled by default");

        // The Accept the attacker would advertise: a real 32-byte iroh node id
        // (so `node_addr_from_hints` builds a `NodeAddr` and the dial spawns)
        // pointing at an unreachable address that never registers.
        let node_id = LocalEndpointFactory::secret_key(0x0BAD_C0DE)
            .public()
            .as_bytes()
            .to_vec();
        let peer_id = ZakuraPeerId::new(node_id.clone()).expect("32-byte node id is valid");
        let direct_addresses = vec![b"192.0.2.1:1".to_vec()];

        let connector =
            crate::zakura::ZakuraHandshakeConnector::new_with_endpoint(endpoint.clone());
        let upgraded = connector
            .spawn_zakura_dial_to_hints_and_wait(&peer_id, &node_id, &direct_addresses)
            .await;

        assert!(
            !upgraded,
            "an unreachable upgrade peer must not report a completed hand-off",
        );
        assert!(
            endpoint
                .upgrade_dials
                .lock()
                .expect("Zakura upgrade dial registry mutex is never poisoned")
                .is_empty(),
            "a failed legacy upgrade leaked a maintained native dial / upgrade_dials entry",
        );

        endpoint.shutdown().await;
        Ok(())
    }

    #[tokio::test]
    async fn registry_routes_legacy_and_header_sync_and_drops_unknown() -> Result<(), BoxError> {
        let _guard = zebra_test::init();
        let shutdown = CancellationToken::new();
        let startup = header_sync_startup(shutdown.clone());
        let (header_sync, mut actions, task) = spawn_header_sync_reactor(startup)?;
        let recorder = Arc::new(RecordingService::default());
        let supervisor = ZakuraSupervisorHandle::new(1);
        let registry = service_registry(
            &supervisor,
            Some(header_sync.clone()),
            None,
            ZakuraBlockSyncConfig::default(),
            recorder.clone(),
            test_discovery_service(&supervisor),
        )?;
        let peer = test_peer(6);

        let (session, _recv) = header_sync_test_session(peer.clone());
        header_sync
            .send(HeaderSyncEvent::PeerConnected(session))
            .await?;
        assert!(matches!(
            next_header_sync_action(&mut actions).await,
            HeaderSyncAction::SendMessage {
                msg: HeaderSyncMessage::Status(_),
                ..
            }
        ));

        let gossip_frame = Frame {
            message_type: 11,
            flags: 0,
            payload: Vec::new(),
        };
        let request_frame = Frame {
            message_type: 12,
            flags: 0,
            payload: Vec::new(),
        };
        let get_headers_frame = HeaderSyncMessage::GetHeaders {
            start_height: block::Height(1),
            count: 1,
        }
        .encode_frame()?;

        let (gossip_result, request_result, header_sync_result) = tokio::join!(
            async { registry.deliver(peer.clone(), LEGACY_GOSSIP_STREAM_KIND, gossip_frame) },
            async { registry.deliver(peer.clone(), LEGACY_REQUEST_STREAM_KIND, request_frame) },
            async { registry.deliver(peer.clone(), HEADER_SYNC_STREAM_KIND, get_headers_frame) },
        );

        gossip_result?;
        request_result?;
        header_sync_result?;
        registry.deliver(
            peer.clone(),
            99,
            Frame {
                message_type: 13,
                flags: 0,
                payload: Vec::new(),
            },
        )?;

        let action = next_header_sync_action(&mut actions).await;
        assert!(
            matches!(
                action,
                HeaderSyncAction::Misbehavior {
                    reason: HeaderSyncMisbehavior::GetHeadersSpam,
                    ..
                }
            ),
            "kind-5 GetHeaders must reach the header-sync reactor, got {action:?}"
        );

        let deliveries = recorder.deliveries();
        assert_eq!(deliveries.len(), 2);
        assert!(deliveries.iter().any(|(_, kind, message_type)| *kind
            == LEGACY_GOSSIP_STREAM_KIND
            && *message_type == 11));
        assert!(deliveries.iter().any(|(_, kind, message_type)| *kind
            == LEGACY_REQUEST_STREAM_KIND
            && *message_type == 12));
        assert!(!deliveries
            .iter()
            .any(|(_, kind, _)| *kind == HEADER_SYNC_STREAM_KIND));

        let rejected = registry
            .request(
                peer,
                HEADER_SYNC_STREAM_KIND,
                99,
                LOCAL_MAX_CONTROL_FRAME_BYTES,
                LOCAL_MAX_CONTROL_FRAME_BYTES,
                Frame {
                    message_type: 1,
                    flags: 0,
                    payload: Vec::new(),
                },
            )
            .await;
        assert!(matches!(rejected, Err(SinkReject::Protocol(_))));

        shutdown.cancel();
        task.await?;
        Ok(())
    }

    #[tokio::test]
    async fn malformed_header_sync_frame_is_protocol_reject_when_reactor_queue_closed(
    ) -> Result<(), BoxError> {
        let shutdown = CancellationToken::new();
        let startup = header_sync_startup(shutdown.clone());
        let (header_sync, _actions, task) = spawn_header_sync_reactor(startup)?;
        let service = HeaderSyncService::new(header_sync);
        let peer = test_peer(11);

        shutdown.cancel();
        task.await?;

        let valid_status_frame =
            HeaderSyncMessage::Status(status_at_genesis(&Network::Mainnet)).encode_frame()?;
        let valid_result =
            service.deliver_frame(peer.clone(), HEADER_SYNC_STREAM_KIND, valid_status_frame);
        assert!(
            matches!(valid_result, Err(SinkReject::Local(_))),
            "valid stream-5 frames depend on local reactor queue availability"
        );

        let malformed_frame = Frame {
            message_type: u16::from(MSG_HS_STATUS),
            flags: 0,
            payload: Vec::new(),
        };
        let malformed_result =
            service.deliver_frame(peer, HEADER_SYNC_STREAM_KIND, malformed_frame);
        assert!(
            matches!(malformed_result, Err(SinkReject::Protocol(_))),
            "malformed stream-5 frames must disconnect independently of reactor queue availability"
        );

        Ok(())
    }

    #[tokio::test]
    async fn header_sync_peer_connected_has_ready_outbound_source() -> Result<(), BoxError> {
        let _guard = zebra_test::init();
        let shutdown = CancellationToken::new();
        let startup = header_sync_startup(shutdown.clone());
        let (header_sync, mut actions, reactor_task) = spawn_header_sync_reactor(startup)?;
        let service = HeaderSyncService::new(header_sync);
        let peer = test_peer(12);
        let cancel_token = CancellationToken::new();
        let (_inbound_tx, inbound_rx) = crate::zakura::framed_channel(1);
        let (outbound_tx, mut outbound_rx) = crate::zakura::framed_channel(1);

        let mut streams = HashMap::new();
        streams.insert(HEADER_SYNC_STREAM_KIND, (inbound_rx, outbound_tx));
        service.add_peer(Peer::new(
            peer.clone(),
            None,
            ZAKURA_CAP_HEADER_SYNC,
            streams,
            cancel_token.clone(),
        ));

        let received = tokio::time::timeout(Duration::from_secs(1), outbound_rx.recv())
            .await
            .expect("header-sync outbound source is immediately ready")
            .expect("header-sync outbound receiver stays open");
        assert_eq!(received.message_type, u16::from(MSG_HS_STATUS));

        assert!(matches!(
            next_header_sync_action(&mut actions).await,
            HeaderSyncAction::SendMessage {
                msg: HeaderSyncMessage::Status(_),
                ..
            }
        ));

        cancel_token.cancel();
        service.remove_peer(&peer);
        shutdown.cancel();
        reactor_task.await?;
        Ok(())
    }

    #[tokio::test]
    async fn lazy_escalation_service_demand_respects_directional_caps() -> Result<(), BoxError> {
        let _guard = zebra_test::init();
        let shutdown = CancellationToken::new();
        let mut startup = header_sync_startup(shutdown.clone());
        startup.config.peer_limits = ServicePeerLimits {
            max_inbound_peers: 0,
            max_outbound_peers: 1,
            ..ServicePeerLimits::default()
        };
        let (header_sync, _actions, reactor_task) = spawn_header_sync_reactor(startup)?;
        let service = HeaderSyncService::new(header_sync);
        let peer = test_peer(17);

        assert!(
            !service.wants_peer(&peer, ZAKURA_CAP_HEADER_SYNC, ServicePeerDirection::Inbound,),
            "a full inbound cap must not open a new header-sync stream"
        );
        assert!(
            service.wants_peer(
                &peer,
                ZAKURA_CAP_HEADER_SYNC,
                ServicePeerDirection::Outbound,
            ),
            "an outbound slot should still allow lazy header-sync escalation"
        );

        shutdown.cancel();
        reactor_task.await?;
        Ok(())
    }

    #[tokio::test]
    async fn registry_add_remove_updates_header_sync_peers() -> Result<(), BoxError> {
        let _guard = zebra_test::init();
        let shutdown = CancellationToken::new();
        let startup = header_sync_startup(shutdown.clone());
        let (header_sync, mut actions, reactor_task) = spawn_header_sync_reactor(startup)?;
        let peer = test_peer(7);
        let supervisor = ZakuraSupervisorHandle::new(1);
        let registry = service_registry(
            &supervisor,
            Some(header_sync.clone()),
            None,
            ZakuraBlockSyncConfig::default(),
            Arc::new(RecordingService::default()),
            test_discovery_service(&supervisor),
        )?;
        let (_inbound_tx, inbound_rx) = crate::zakura::framed_channel(1);
        let (outbound_tx, _outbound_rx) = crate::zakura::framed_channel(1);
        let mut streams = HashMap::new();
        streams.insert(HEADER_SYNC_STREAM_KIND, (inbound_rx, outbound_tx));
        registry.add_peer(Peer::new(
            peer.clone(),
            None,
            ZAKURA_CAP_HEADER_SYNC,
            streams,
            CancellationToken::new(),
        ));
        assert!(matches!(
            next_header_sync_action(&mut actions).await,
            HeaderSyncAction::SendMessage {
                msg: HeaderSyncMessage::Status(_),
                ..
            }
        ));

        header_sync
            .send(HeaderSyncEvent::WireMessage {
                peer: peer.clone(),
                msg: HeaderSyncMessage::Status(status_at_genesis(&Network::Mainnet)),
            })
            .await?;
        registry.remove_peer(&peer, ZAKURA_CAP_HEADER_SYNC);
        tokio::time::sleep(Duration::from_millis(50)).await;
        header_sync
            .send(HeaderSyncEvent::WireMessage {
                peer: peer.clone(),
                msg: HeaderSyncMessage::GetHeaders {
                    start_height: block::Height(1),
                    count: 1,
                },
            })
            .await?;

        let action = next_header_sync_action(&mut actions).await;
        assert!(
            matches!(
                action,
                HeaderSyncAction::Misbehavior {
                    reason: HeaderSyncMisbehavior::GetHeadersSpam,
                    ..
                }
            ),
            "deregistered peer must be removed from header-sync state, got {action:?}"
        );

        shutdown.cancel();
        reactor_task.await?;
        Ok(())
    }

    #[tokio::test]
    async fn discovery_admission_is_independent_when_header_sync_is_full() -> Result<(), BoxError> {
        let _guard = zebra_test::init();
        let shutdown = CancellationToken::new();
        let mut startup = header_sync_startup(shutdown.clone());
        startup.config.peer_limits = ServicePeerLimits {
            max_inbound_peers: 0,
            ..ServicePeerLimits::default()
        };
        let (header_sync, mut actions, reactor_task) = spawn_header_sync_reactor(startup)?;
        let supervisor = ZakuraSupervisorHandle::new(1);
        let (discovery_handle, discovery_service) = test_discovery_service_with_peer_limits(
            &supervisor,
            ServicePeerLimits {
                max_inbound_peers: 1,
                ..ServicePeerLimits::default()
            },
        );
        let registry = service_registry(
            &supervisor,
            Some(header_sync.clone()),
            None,
            ZakuraBlockSyncConfig::default(),
            Arc::new(RecordingService::default()),
            discovery_service,
        )?;
        let peer_node_id = SecretKey::from_bytes(&[13u8; 32]).public();
        let peer = ZakuraPeerId::new(peer_node_id.as_bytes().to_vec())?;
        let (_hs_inbound_tx, hs_inbound_rx) = crate::zakura::framed_channel(1);
        let (hs_outbound_tx, mut hs_outbound_rx) = crate::zakura::framed_channel(1);
        let (_discovery_inbound_tx, discovery_inbound_rx) = crate::zakura::framed_channel(1);
        let (discovery_outbound_tx, mut discovery_outbound_rx) = crate::zakura::framed_channel(4);
        let streams = HashMap::from([
            (HEADER_SYNC_STREAM_KIND, (hs_inbound_rx, hs_outbound_tx)),
            (
                DISCOVERY_STREAM_KIND,
                (discovery_inbound_rx, discovery_outbound_tx),
            ),
        ]);

        registry.add_peer(Peer::new(
            peer.clone(),
            None,
            ZAKURA_CAP_HEADER_SYNC | ZAKURA_CAP_DISCOVERY,
            streams,
            CancellationToken::new(),
        ));

        tokio::time::timeout(Duration::from_secs(1), async {
            let mut snapshots = discovery_handle.subscribe_peer_snapshot();
            while snapshots.borrow().inbound_peers != 1 {
                snapshots
                    .changed()
                    .await
                    .expect("discovery snapshot channel stays open");
            }
        })
        .await
        .expect("discovery admits while header-sync is full");

        assert_eq!(header_sync.peer_snapshot().inbound_peers, 0);
        assert_eq!(header_sync.peer_snapshot().inbound_slots_free, 0);
        assert!(
            !matches!(
                tokio::time::timeout(Duration::from_millis(100), hs_outbound_rx.recv()).await,
                Ok(Some(_))
            ),
            "header-sync rejected peer must not receive Status"
        );
        let discovery_frame =
            tokio::time::timeout(Duration::from_secs(1), discovery_outbound_rx.recv())
                .await
                .expect("discovery source sends its normal hello")
                .expect("discovery outbound stream stays open");
        assert_eq!(discovery_frame.message_type, 1);

        while let Ok(Some(action)) =
            tokio::time::timeout(Duration::from_millis(50), actions.recv()).await
        {
            assert!(
                !matches!(action, HeaderSyncAction::Misbehavior { peer: action_peer, .. } if action_peer == peer),
                "header-sync cap rejection must not score peer misbehavior"
            );
        }

        registry.remove_peer(&peer, ZAKURA_CAP_HEADER_SYNC | ZAKURA_CAP_DISCOVERY);
        tokio::time::timeout(Duration::from_secs(1), async {
            let mut snapshots = discovery_handle.subscribe_peer_snapshot();
            while snapshots.borrow().inbound_peers != 0 {
                snapshots
                    .changed()
                    .await
                    .expect("discovery snapshot channel stays open");
            }
        })
        .await
        .expect("discovery peer state is removed");

        shutdown.cancel();
        reactor_task.await?;
        Ok(())
    }

    #[tokio::test]
    async fn header_sync_admission_is_independent_when_discovery_is_full() -> Result<(), BoxError> {
        let _guard = zebra_test::init();
        let shutdown = CancellationToken::new();
        let mut startup = header_sync_startup(shutdown.clone());
        startup.config.peer_limits = ServicePeerLimits {
            max_inbound_peers: 1,
            ..ServicePeerLimits::default()
        };
        let (header_sync, mut actions, reactor_task) = spawn_header_sync_reactor(startup)?;
        let supervisor = ZakuraSupervisorHandle::new(1);
        let (discovery_handle, discovery_service) = test_discovery_service_with_peer_limits(
            &supervisor,
            ServicePeerLimits {
                max_inbound_peers: 0,
                ..ServicePeerLimits::default()
            },
        );
        let registry = service_registry(
            &supervisor,
            Some(header_sync.clone()),
            None,
            ZakuraBlockSyncConfig::default(),
            Arc::new(RecordingService::default()),
            discovery_service,
        )?;
        let peer_node_id = SecretKey::from_bytes(&[14u8; 32]).public();
        let peer = ZakuraPeerId::new(peer_node_id.as_bytes().to_vec())?;
        let (_hs_inbound_tx, hs_inbound_rx) = crate::zakura::framed_channel(1);
        let (hs_outbound_tx, mut hs_outbound_rx) = crate::zakura::framed_channel(1);
        let (_discovery_inbound_tx, discovery_inbound_rx) = crate::zakura::framed_channel(1);
        let (discovery_outbound_tx, mut discovery_outbound_rx) = crate::zakura::framed_channel(1);
        let streams = HashMap::from([
            (HEADER_SYNC_STREAM_KIND, (hs_inbound_rx, hs_outbound_tx)),
            (
                DISCOVERY_STREAM_KIND,
                (discovery_inbound_rx, discovery_outbound_tx),
            ),
        ]);

        registry.add_peer(Peer::new(
            peer.clone(),
            None,
            ZAKURA_CAP_HEADER_SYNC | ZAKURA_CAP_DISCOVERY,
            streams,
            CancellationToken::new(),
        ));

        let header_frame = tokio::time::timeout(Duration::from_secs(1), hs_outbound_rx.recv())
            .await
            .expect("header-sync sends immediate Status")
            .expect("header-sync outbound stream stays open");
        assert_eq!(header_frame.message_type, u16::from(MSG_HS_STATUS));
        assert!(matches!(
            next_header_sync_action(&mut actions).await,
            HeaderSyncAction::SendMessage {
                peer: action_peer,
                msg: HeaderSyncMessage::Status(_),
            } if action_peer == peer
        ));
        assert_eq!(header_sync.peer_snapshot().inbound_peers, 1);
        assert_eq!(discovery_handle.peer_snapshot().inbound_peers, 0);
        assert!(
            !matches!(
                tokio::time::timeout(Duration::from_millis(100), discovery_outbound_rx.recv())
                    .await,
                Ok(Some(_))
            ),
            "discovery rejected peer must not receive discovery source messages"
        );

        registry.remove_peer(&peer, ZAKURA_CAP_HEADER_SYNC | ZAKURA_CAP_DISCOVERY);
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert_eq!(header_sync.peer_snapshot().inbound_peers, 0);

        shutdown.cancel();
        reactor_task.await?;
        Ok(())
    }

    #[tokio::test]
    async fn supervisor_disconnect_peer_cancels_registered_token() -> Result<(), BoxError> {
        let supervisor = ZakuraSupervisorHandle::new(4);
        let peer = test_peer(8);
        let disconnect_token = CancellationToken::new();
        register_test_peer(&supervisor, peer.clone(), disconnect_token.clone()).await;

        assert!(supervisor.disconnect_peer(&peer).await);
        tokio::time::timeout(Duration::from_secs(1), disconnect_token.cancelled())
            .await
            .expect("disconnect token is cancelled promptly");
        assert!(!supervisor.disconnect_peer(&test_peer(9)).await);

        Ok(())
    }

    #[tokio::test(start_paused = true)]
    async fn duplicate_evicts_stale_incumbent_but_keeps_fresh_one() -> Result<(), BoxError> {
        // A duplicate connection for an identity that already has one either
        // means the peer restarted (incumbent is a dead, stale connection that
        // should be evicted so the redial can reclaim the slot) or that two
        // connections raced at startup (simultaneous open, both fresh, must NOT
        // be evicted or they flap). Incumbent age distinguishes the two.
        let supervisor = ZakuraSupervisorHandle::new(4);

        async fn register_duplicate(
            supervisor: &ZakuraSupervisorHandle,
            peer: &ZakuraPeerId,
            token: CancellationToken,
        ) -> ZakuraRegistration {
            let (outbound_tx, _outbound_rx) = mpsc::channel(1);
            let outbound_handle = ZakuraPeerHandle::new_for_tests(peer.clone(), outbound_tx);
            supervisor
                .register(
                    peer.clone(),
                    None,
                    [peer.as_bytes()[0]; TRANSCRIPT_HASH_BYTES],
                    outbound_handle,
                    token,
                    ZAKURA_CAP_LEGACY_GOSSIP | ZAKURA_CAP_HEADER_SYNC,
                )
                .await
        }

        // Fresh incumbent: an immediate duplicate is a simultaneous-open race and
        // must not evict it.
        let fresh_peer = test_peer(8);
        let fresh_incumbent = CancellationToken::new();
        register_test_peer(&supervisor, fresh_peer.clone(), fresh_incumbent.clone()).await;
        let registration =
            register_duplicate(&supervisor, &fresh_peer, CancellationToken::new()).await;
        assert!(matches!(registration, ZakuraRegistration::Duplicate { .. }));
        assert!(
            !fresh_incumbent.is_cancelled(),
            "a young incumbent is kept so simultaneous-open races do not flap",
        );

        // Stale incumbent: once it is older than the threshold, a duplicate
        // evicts it so a restarted peer's redial can reclaim the slot.
        let stale_peer = test_peer(9);
        let stale_incumbent = CancellationToken::new();
        register_test_peer(&supervisor, stale_peer.clone(), stale_incumbent.clone()).await;
        tokio::time::advance(ZAKURA_DUPLICATE_EVICT_MIN_AGE + Duration::from_secs(1)).await;
        let newcomer = CancellationToken::new();
        let registration = register_duplicate(&supervisor, &stale_peer, newcomer.clone()).await;
        assert!(matches!(registration, ZakuraRegistration::Duplicate { .. }));
        assert!(
            stale_incumbent.is_cancelled(),
            "a stale incumbent is evicted so the restarted peer's redial reclaims the slot",
        );
        assert!(
            !newcomer.is_cancelled(),
            "the rejected newcomer's token is never registered, so it is left to redial",
        );

        Ok(())
    }

    // SECURITY AUDIT (candidate claude-per-ip-cap-blocks-stale-duplicate-eviction /
    // codex-ip-cap-blocks-stale-duplicate-eviction /
    // subset-admission-identity-state-ip-cap-blocks-stale-duplicate-eviction):
    // SR-4 liveness.
    //
    // Outbound/native-direct dials register with `remote_ip = Some(ip)`, so the
    // per-IP cap precheck runs. A same-peer redial from an IP already at the cap
    // must NOT be rejected as a resource limit before the duplicate stale-eviction
    // path runs: the incumbent already occupies the only per-IP slot, so a duplicate
    // for the same identity cannot consume an additional slot. If the precheck
    // rejects it first, a dead incumbent keeps its own peer blocked until the QUIC
    // idle timeout (~150s) instead of being evicted in milliseconds.
    #[tokio::test(start_paused = true)]
    async fn same_peer_duplicate_at_per_ip_cap_still_evicts_stale_incumbent() -> Result<(), BoxError>
    {
        async fn register_from_ip(
            supervisor: &ZakuraSupervisorHandle,
            peer: &ZakuraPeerId,
            ip: IpAddr,
            token: CancellationToken,
        ) -> ZakuraRegistration {
            let (outbound_tx, _outbound_rx) = mpsc::channel(1);
            let outbound_handle = ZakuraPeerHandle::new_for_tests(peer.clone(), outbound_tx);
            supervisor
                .register(
                    peer.clone(),
                    Some(ip),
                    [peer.as_bytes()[0]; TRANSCRIPT_HASH_BYTES],
                    outbound_handle,
                    token,
                    ZAKURA_CAP_LEGACY_GOSSIP | ZAKURA_CAP_HEADER_SYNC,
                )
                .await
        }

        let supervisor = ZakuraSupervisorHandle::new(1);
        let ip: IpAddr = "203.0.113.9".parse().expect("test ip parses");
        let peer = test_peer(42);

        // Incumbent: a native-direct dial registers from the IP and fills the
        // cap-1 per-IP slot.
        let incumbent = CancellationToken::new();
        let registration = register_from_ip(&supervisor, &peer, ip, incumbent.clone()).await;
        assert!(
            matches!(registration, ZakuraRegistration::Registered { .. }),
            "the first connection from the IP registers",
        );

        // The incumbent ages past the stale-eviction threshold: it is now a likely
        // dead connection left behind by a peer restart.
        tokio::time::advance(ZAKURA_DUPLICATE_EVICT_MIN_AGE + Duration::from_secs(1)).await;

        // The same peer redials from the same IP. The IP is already at cap 1, but
        // the duplicate must still reach the stale-eviction path and cancel the dead
        // incumbent so the redial reclaims the slot in milliseconds.
        let newcomer = CancellationToken::new();
        let registration = register_from_ip(&supervisor, &peer, ip, newcomer.clone()).await;
        assert!(
            matches!(registration, ZakuraRegistration::Duplicate { .. }),
            "a same-peer redial from a capped IP must reach duplicate handling, not be \
             rejected as a resource limit before stale eviction can run",
        );
        assert!(
            incumbent.is_cancelled(),
            "the stale incumbent must be evicted so the restarted peer's redial reclaims \
             the slot in milliseconds instead of waiting for the QUIC idle timeout",
        );
        assert!(
            !newcomer.is_cancelled(),
            "the rejected newcomer's token is never registered, so it is left to redial",
        );

        Ok(())
    }

    #[tokio::test]
    async fn header_sync_misbehavior_action_disconnects_peer() -> Result<(), BoxError> {
        let _guard = zebra_test::init();
        let reactor_shutdown = CancellationToken::new();
        let startup = header_sync_startup(reactor_shutdown.clone());
        let (header_sync, _reactor_actions, reactor_task) = spawn_header_sync_reactor(startup)?;
        let supervisor = ZakuraSupervisorHandle::new(4);
        let peer = test_peer(10);
        let disconnect_token = CancellationToken::new();
        register_test_peer(&supervisor, peer.clone(), disconnect_token.clone()).await;

        let (actions_tx, actions_rx) = mpsc::channel(4);
        let driver_shutdown = CancellationToken::new();
        let driver_task = tokio::spawn(drive_header_sync_actions(
            actions_rx,
            header_sync,
            supervisor,
            driver_shutdown.clone(),
        ));
        actions_tx
            .send(HeaderSyncAction::Misbehavior {
                peer,
                reason: HeaderSyncMisbehavior::MalformedMessage,
            })
            .await?;

        tokio::time::timeout(Duration::from_secs(1), disconnect_token.cancelled())
            .await
            .expect("misbehavior action cancels the registered connection");

        driver_shutdown.cancel();
        driver_task.await?;
        reactor_shutdown.cancel();
        reactor_task.await?;
        Ok(())
    }

    #[tokio::test]
    async fn stream_cancel_closes_ordered_worker_without_connection_cancel() -> Result<(), BoxError>
    {
        const ALPN: &[u8] = b"/zakura/testkit/stream-cancel/0";

        let _guard = zebra_test::init();
        let server = LocalEndpointFactory::new().endpoint(50).await?;
        let (conn_tx, mut conn_rx) = mpsc::channel(1);
        let (stream_tx, mut stream_rx) = mpsc::channel(2);
        let router = Router::builder(server)
            .accept(
                ALPN,
                CaptureConnection {
                    connection_tx: conn_tx,
                    stream_tx,
                },
            )
            .spawn();
        let client = LocalEndpointFactory::new().endpoint(51).await?;
        let server_addr = router.endpoint().node_addr().initialized().await;
        client.add_node_addr(server_addr.clone())?;

        let client_conn = timeout(Duration::from_secs(10), client.connect(server_addr, ALPN))
            .await
            .expect("client connects to loopback capture endpoint")?;
        let server_conn = timeout(Duration::from_secs(10), conn_rx.recv())
            .await
            .expect("server connection is captured")
            .expect("capture handler sends the accepted connection");
        drop(server_conn);
        let (mut client_send, _client_recv) =
            timeout(Duration::from_secs(1), client_conn.open_bi())
                .await
                .expect("client opens the worker stream")?;
        let test_frame = Frame {
            message_type: 1,
            flags: 0,
            payload: Vec::new(),
        }
        .encode(LOCAL_MAX_CONTROL_FRAME_BYTES)?;
        timeout(Duration::from_secs(1), client_send.write_all(&test_frame))
            .await
            .expect("client writes the worker stream frame")?;
        let (server_send, server_recv) = timeout(Duration::from_secs(1), stream_rx.recv())
            .await
            .expect("server accepts the worker stream")
            .expect("capture handler sends the worker stream");

        let mut limits = test_connection_limits();
        limits.idle_timeout = Duration::from_millis(50);
        let stream_kind = DISCOVERY_STREAM_KIND;
        let connection_token = CancellationToken::new();
        let stream_token = connection_token.child_token();
        let (inbound_tx, _inbound_rx) = mpsc::channel(1);
        let (outbound_tx, outbound_rx) = mpsc::channel(1);
        let (freshness_tx, _freshness_rx) = watch::channel(Instant::now());
        let permit = Arc::new(Semaphore::new(1))
            .try_acquire_owned()
            .expect("test semaphore starts with one permit");
        let context = StreamWorkerContext {
            trace: ZakuraTrace::noop(),
            conn: ZakuraConnTrace::without_peer(1),
            peer_id: test_peer(55),
            stream_id: 1,
            _permit: permit,
            limits,
            message_bucket: Arc::new(std::sync::Mutex::new(TokenBucket::new(128))),
            connection_token: connection_token.clone(),
            stream_token: stream_token.clone(),
            freshness_tx,
        };
        let prelude = StreamPrelude {
            magic: STREAM_PRELUDE_MAGIC,
            stream_kind,
            stream_version: ZAKURA_STREAM_VERSION_1,
            request_id: None,
            max_frame_bytes: app_frame_cap_for_stream_kind(&limits, stream_kind),
        };

        let worker = tokio::spawn(persistent_stream_worker(
            server_send,
            server_recv,
            prelude,
            context,
            inbound_tx,
            outbound_rx,
            1,
        ));

        drop(outbound_tx);
        stream_token.cancel();
        timeout(Duration::from_secs(1), worker)
            .await
            .expect("stream cancellation stops the worker promptly")?;
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(
            !connection_token.is_cancelled(),
            "stream-local cancellation must not cancel the shared connection"
        );

        let (mut sibling_send, _sibling_recv) =
            timeout(Duration::from_secs(1), client_conn.open_bi())
                .await
                .expect("connection survives to open a sibling stream")?;
        timeout(Duration::from_secs(1), sibling_send.write_all(&test_frame))
            .await
            .expect("client writes the sibling stream frame")?;
        timeout(Duration::from_secs(1), stream_rx.recv())
            .await
            .expect("server accepts the sibling stream")
            .expect("capture handler sends the sibling stream");

        client_conn.close(0u32.into(), b"done");
        client.close().await;
        router.shutdown().await?;
        Ok(())
    }

    /// Regression for `claude-outbound-write-ignores-message-cap` (persistent
    /// ordered-stream facet).
    ///
    /// `write_ordered_frame` sized outbound persistent-stream frames against the
    /// negotiated frame cap only. The handshake clamps `max_frame_bytes` and
    /// `max_message_bytes` independently, so a peer can negotiate a small message
    /// cap with a large frame cap. A persistent service that queues a frame
    /// larger than that message cap then has it encoded and written, and the peer
    /// rejects it as oversize and disconnects us, wasting the encode/write.
    /// `write_ordered_frame` must mirror `write_response_frame` and reject an
    /// over-message-cap frame locally before encoding/writing it, while still
    /// writing frames within the cap.
    #[tokio::test]
    async fn write_ordered_frame_rejects_payload_over_message_cap() -> Result<(), BoxError> {
        const ALPN: &[u8] = b"/zakura/testkit/ordered-message-cap/0";

        let _guard = zebra_test::init();
        let server = LocalEndpointFactory::new().endpoint(74).await?;
        let (conn_tx, _conn_rx) = mpsc::channel(1);
        let (stream_tx, _stream_rx) = mpsc::channel(2);
        let router = Router::builder(server)
            .accept(
                ALPN,
                CaptureConnection {
                    connection_tx: conn_tx,
                    stream_tx,
                },
            )
            .spawn();
        let client = LocalEndpointFactory::new().endpoint(75).await?;
        let server_addr = router.endpoint().node_addr().initialized().await;
        client.add_node_addr(server_addr.clone())?;

        let client_conn = timeout(Duration::from_secs(10), client.connect(server_addr, ALPN))
            .await
            .expect("client connects to the ordered-message-cap endpoint")?;
        let (mut send, _recv) = timeout(Duration::from_secs(1), client_conn.open_bi())
            .await
            .expect("client opens an ordered stream")?;

        // A small negotiated message cap with a large frame cap, as the handshake
        // permits.
        let mut limits = test_connection_limits();
        limits.max_message_bytes = 256;
        let stream_kind = DISCOVERY_STREAM_KIND;

        // A payload above the message cap but well within the frame cap, so only
        // the message cap can reject it.
        let oversized = Frame {
            message_type: 1,
            flags: 0,
            payload: vec![0xab; 4096],
        };
        assert!(
            oversized.payload.len() <= app_frame_cap_for_stream_kind(&limits, stream_kind) as usize,
            "test payload must fit the frame cap so only the message cap can reject it"
        );

        let result = write_ordered_frame(&mut send, oversized, limits, stream_kind).await;
        assert!(
            result.is_err(),
            "write_ordered_frame must reject a payload over the negotiated max_message_bytes \
             before encoding/writing it, mirroring write_response_frame; got {result:?}"
        );

        // A payload within the negotiated message cap must still be written.
        let within_cap = Frame {
            message_type: 1,
            flags: 0,
            payload: vec![0xcd; 128],
        };
        write_ordered_frame(&mut send, within_cap, limits, stream_kind)
            .await
            .expect("a frame within the negotiated message cap must still be written");

        client_conn.close(0u32.into(), b"done");
        client.close().await;
        router.shutdown().await?;
        Ok(())
    }

    // Regression for `claude-control-payload-late-hard-cap`: the native control
    // hello/ack reads passed the configured `max_control_frame_bytes` (1 MiB
    // default) to `read_control_payload`, so a peer could force allocation/read
    // of a control payload between the 16 KiB hard cap and 1 MiB before
    // ZakuraControlHello/Ack::decode rejected it. The reader must instead reject
    // an oversized control length on the length prefix alone, before reading the
    // body, while still accepting payloads within the 16 KiB hard cap.
    #[tokio::test]
    async fn read_control_payload_enforces_hard_cap_before_reading_body() -> Result<(), BoxError> {
        const ALPN: &[u8] = b"/zakura/testkit/control-hard-cap/0";

        let _guard = zebra_test::init();
        let server = LocalEndpointFactory::new().endpoint(70).await?;
        let (conn_tx, _conn_rx) = mpsc::channel(1);
        let (stream_tx, mut stream_rx) = mpsc::channel(2);
        let router = Router::builder(server)
            .accept(
                ALPN,
                CaptureConnection {
                    connection_tx: conn_tx,
                    stream_tx,
                },
            )
            .spawn();
        let client = LocalEndpointFactory::new().endpoint(71).await?;
        let server_addr = router.endpoint().node_addr().initialized().await;
        client.add_node_addr(server_addr.clone())?;

        let client_conn = timeout(Duration::from_secs(10), client.connect(server_addr, ALPN))
            .await
            .expect("client connects to the control-hard-cap endpoint")?;

        // A control length above the 16 KiB hard cap but below the 1 MiB frame
        // cap the production responder/initiator pass. The body is never sent: a
        // correct reader must reject on the length prefix alone.
        let oversized_len =
            u32::try_from(MAX_CONTROL_PAYLOAD_BYTES + 1).expect("control hard cap + 1 fits in u32");
        let (mut over_send, _over_recv) = timeout(Duration::from_secs(1), client_conn.open_bi())
            .await
            .expect("client opens the oversized control stream")?;
        timeout(
            Duration::from_secs(1),
            over_send.write_all(&oversized_len.to_le_bytes()),
        )
        .await
        .expect("client writes the oversized control length")?;
        let _ = over_send.finish();

        let (_over_server_send, mut over_server_recv) =
            timeout(Duration::from_secs(1), stream_rx.recv())
                .await
                .expect("server accepts the oversized control stream")
                .expect("capture handler forwards the oversized control stream");

        // Pass exactly what production passes: the configured frame cap (1 MiB),
        // NOT the 16 KiB hard cap. The fix clamps internally to the hard cap.
        let oversized = read_control_payload(
            &mut over_server_recv,
            LOCAL_MAX_CONTROL_FRAME_BYTES,
            Duration::from_secs(2),
        )
        .await;
        assert!(
            matches!(oversized, Err(ZakuraHandlerError::Oversize)),
            "a control length over the 16 KiB hard cap must be rejected as Oversize \
             on the length prefix, before the body is read; got {oversized:?}"
        );

        // A control payload within the hard cap must still be read normally, so
        // the clamp does not break legitimate sub-16 KiB control frames.
        let valid_body = vec![0xa5u8; 128];
        let valid_len = u32::try_from(valid_body.len()).expect("128 fits in u32");
        let (mut ok_send, _ok_recv) = timeout(Duration::from_secs(1), client_conn.open_bi())
            .await
            .expect("client opens the in-cap control stream")?;
        timeout(
            Duration::from_secs(1),
            ok_send.write_all(&valid_len.to_le_bytes()),
        )
        .await
        .expect("client writes the in-cap control length")?;
        timeout(Duration::from_secs(1), ok_send.write_all(&valid_body))
            .await
            .expect("client writes the in-cap control body")?;
        let _ = ok_send.finish();

        let (_ok_server_send, mut ok_server_recv) =
            timeout(Duration::from_secs(1), stream_rx.recv())
                .await
                .expect("server accepts the in-cap control stream")
                .expect("capture handler forwards the in-cap control stream");
        let read_back = read_control_payload(
            &mut ok_server_recv,
            LOCAL_MAX_CONTROL_FRAME_BYTES,
            Duration::from_secs(2),
        )
        .await
        .expect("a control payload within the hard cap is read");
        assert_eq!(
            read_back, valid_body,
            "an in-cap control payload must round-trip unchanged"
        );

        client_conn.close(0u32.into(), b"done");
        client.close().await;
        router.shutdown().await?;
        Ok(())
    }

    // claude-late-message-cap-allocation: read_frame checks only
    // frame_len > max_frame_bytes before `vec![0; payload_len]`, while the smaller
    // max_message_bytes is enforced later in admit_inbound_message. A peer can
    // negotiate max_frame_bytes > max_message_bytes (the caps are clamped
    // independently), so on every admitted ordered/request stream it could force
    // read_frame to allocate and read a payload between the two limits before the
    // message cap rejects it. The inbound read path must instead be handed a cap
    // already limited to the message size, so an over-message frame is rejected on
    // its header alone, before the payload is allocated and read.
    #[tokio::test]
    async fn inbound_frame_cap_rejects_over_message_frame_before_reading_payload(
    ) -> Result<(), BoxError> {
        const ALPN: &[u8] = b"/zakura/testkit/late-message-cap/0";
        // The negotiated frame cap is far larger than the message cap: exactly the
        // precondition the finding requires (caps allowed to diverge).
        const MAX_FRAME_BYTES: u32 = 64 * 1024;
        const MAX_MESSAGE_BYTES: u32 = 1024;
        // A payload between the message cap and the frame cap. admit_inbound_message
        // would reject it, but only after read_frame allocated and read it.
        const OVER_MESSAGE_PAYLOAD_LEN: u32 = 2048;
        let stream_kind = LEGACY_GOSSIP_STREAM_KIND;

        let limits = ZakuraConnectionLimits {
            max_frame_bytes: MAX_FRAME_BYTES,
            max_message_bytes: MAX_MESSAGE_BYTES,
            ..test_connection_limits()
        };
        // Production now passes the message-limited inbound cap; the raw
        // application cap (what the unfixed read path used) stays at the frame cap.
        let inbound_cap = inbound_frame_cap_for_stream_kind(&limits, stream_kind);
        let raw_cap = app_frame_cap_for_stream_kind(&limits, stream_kind);
        assert!(
            inbound_cap < raw_cap,
            "the inbound cap must be tighter than the raw frame cap when the caps diverge \
             (inbound_cap={inbound_cap}, raw_cap={raw_cap})"
        );

        let _guard = zebra_test::init();
        let server = LocalEndpointFactory::new().endpoint(72).await?;
        let (conn_tx, _conn_rx) = mpsc::channel(2);
        let (stream_tx, mut stream_rx) = mpsc::channel(4);
        let router = Router::builder(server)
            .accept(
                ALPN,
                CaptureConnection {
                    connection_tx: conn_tx,
                    stream_tx,
                },
            )
            .spawn();
        let client = LocalEndpointFactory::new().endpoint(73).await?;
        let server_addr = router.endpoint().node_addr().initialized().await;
        client.add_node_addr(server_addr.clone())?;

        // A frame header (message_type, flags, payload_len) with no payload bytes.
        let frame_header = |payload_len: u32| -> Vec<u8> {
            let mut header = Vec::with_capacity(FRAME_HEADER_BYTES);
            header.extend_from_slice(&0u16.to_le_bytes());
            header.extend_from_slice(&0u16.to_le_bytes());
            header.extend_from_slice(&payload_len.to_le_bytes());
            header
        };

        // CaptureConnection forwards at most two streams per connection, so the
        // two oversized streams share one connection and the in-cap stream uses
        // another.
        let conn_a = timeout(
            Duration::from_secs(10),
            client.connect(server_addr.clone(), ALPN),
        )
        .await
        .expect("client connects for the oversized streams")?;

        // Stream 1: oversized header read with the message-limited inbound cap.
        // The fix rejects it as Oversize from the header alone, before allocating.
        let (mut over_send, _over_recv) =
            timeout(Duration::from_secs(1), conn_a.open_bi())
                .await
                .expect("client opens the oversized inbound-cap stream")?;
        timeout(
            Duration::from_secs(1),
            over_send.write_all(&frame_header(OVER_MESSAGE_PAYLOAD_LEN)),
        )
        .await
        .expect("client writes the oversized frame header")?;
        let _ = over_send.finish();
        let (_s1_send, mut s1_recv) = timeout(Duration::from_secs(1), stream_rx.recv())
            .await
            .expect("server accepts the oversized inbound-cap stream")
            .expect("capture handler forwards the oversized inbound-cap stream");
        let rejected = read_frame(&mut s1_recv, inbound_cap, Duration::from_secs(2)).await;
        assert!(
            matches!(rejected, Err(ZakuraHandlerError::OversizeFrame { .. })),
            "a frame whose payload exceeds max_message_bytes must be rejected as Oversize \
             on the header alone with the inbound (message-limited) cap, before the payload \
             is allocated and read; got {rejected:?}"
        );

        // Stream 2: the SAME oversized header read with the raw frame cap an
        // unfixed path used. It passes the frame-cap size check, so read_frame
        // allocates `vec![0; payload_len]` and reads the body (failing only because
        // the body was never sent) -- i.e. NOT rejected as Oversize. This is the
        // allocate-before-reject amplification the fix removes.
        let (mut raw_send, _raw_recv) = timeout(Duration::from_secs(1), conn_a.open_bi())
            .await
            .expect("client opens the oversized raw-cap stream")?;
        timeout(
            Duration::from_secs(1),
            raw_send.write_all(&frame_header(OVER_MESSAGE_PAYLOAD_LEN)),
        )
        .await
        .expect("client writes the oversized frame header again")?;
        let _ = raw_send.finish();
        let (_s2_send, mut s2_recv) = timeout(Duration::from_secs(1), stream_rx.recv())
            .await
            .expect("server accepts the oversized raw-cap stream")
            .expect("capture handler forwards the oversized raw-cap stream");
        let allocated = read_frame(&mut s2_recv, raw_cap, Duration::from_secs(2)).await;
        assert!(
            allocated.is_err()
                && !matches!(
                    allocated,
                    Err(ZakuraHandlerError::Oversize | ZakuraHandlerError::OversizeFrame { .. })
                ),
            "with the raw frame cap the same oversized frame passes the size check and \
             read_frame proceeds to allocate/read the payload (it is not rejected as \
             Oversize), proving the message cap is enforced too late; got {allocated:?}"
        );

        // Stream 3 (fresh connection): a frame within the message cap must still
        // round-trip with the inbound cap, so the clamp rejects nothing legitimate.
        let conn_b = timeout(Duration::from_secs(10), client.connect(server_addr, ALPN))
            .await
            .expect("client connects for the in-cap stream")?;
        let valid_payload = vec![0x5au8; (MAX_MESSAGE_BYTES / 2) as usize];
        let mut valid_bytes =
            frame_header(u32::try_from(valid_payload.len()).expect("in-cap payload len fits u32"));
        valid_bytes.extend_from_slice(&valid_payload);
        let (mut ok_send, _ok_recv) = timeout(Duration::from_secs(1), conn_b.open_bi())
            .await
            .expect("client opens the in-cap stream")?;
        timeout(Duration::from_secs(1), ok_send.write_all(&valid_bytes))
            .await
            .expect("client writes the in-cap frame")?;
        let _ = ok_send.finish();
        let (_s3_send, mut s3_recv) = timeout(Duration::from_secs(1), stream_rx.recv())
            .await
            .expect("server accepts the in-cap stream")
            .expect("capture handler forwards the in-cap stream");
        let frame = read_frame(&mut s3_recv, inbound_cap, Duration::from_secs(2))
            .await
            .expect("a frame within the message cap is read with the inbound cap");
        assert_eq!(
            frame.payload, valid_payload,
            "an in-cap frame payload must round-trip unchanged"
        );

        conn_a.close(0u32.into(), b"done");
        conn_b.close(0u32.into(), b"done");
        client.close().await;
        router.shutdown().await?;
        Ok(())
    }

    #[tokio::test]
    async fn invalid_prelude_stream_churn_charges_open_rate_token() -> Result<(), BoxError> {
        // claude-invalid-stream-prelude-churn-bypasses-open-rate: a peer that
        // opens a stream naming an unregistered kind is reset stream-only and the
        // connection is kept. The per-connection stream-open rate token MUST be
        // charged for that protocol-invalid open; otherwise an authenticated peer
        // could churn bad-prelude/unknown-kind/unnegotiated stream opens forever
        // without ever spending open-rate budget. This drives admit_bi_stream
        // over a real Iroh transport and asserts the token was consumed even
        // though the stream itself was rejected as unknown-kind.
        const ALPN: &[u8] = b"/zakura/testkit/open-rate-churn/0";

        let _guard = zebra_test::init();
        let server = LocalEndpointFactory::new().endpoint(90).await?;
        let (conn_tx, _conn_rx) = mpsc::channel(1);
        let (stream_tx, mut stream_rx) = mpsc::channel(2);
        let router = Router::builder(server)
            .accept(
                ALPN,
                CaptureConnection {
                    connection_tx: conn_tx,
                    stream_tx,
                },
            )
            .spawn();
        let client = LocalEndpointFactory::new().endpoint(91).await?;
        let server_addr = router.endpoint().node_addr().initialized().await;
        client.add_node_addr(server_addr.clone())?;
        let client_conn = timeout(Duration::from_secs(10), client.connect(server_addr, ALPN))
            .await
            .expect("client connects to the open-rate-churn endpoint")?;

        // A well-formed prelude that names an unregistered stream kind. It parses
        // cleanly, so admission reaches the unknown-kind reject (a stream-only
        // reset that keeps the connection) rather than the bad-prelude path.
        let prelude = StreamPrelude {
            magic: STREAM_PRELUDE_MAGIC,
            stream_kind: 9,
            stream_version: 1,
            request_id: None,
            max_frame_bytes: 1024,
        };
        let (mut client_send, _client_recv) =
            timeout(Duration::from_secs(1), client_conn.open_bi())
                .await
                .expect("client opens the unknown-kind stream")?;
        timeout(
            Duration::from_secs(1),
            client_send.write_all(&prelude.encode()?),
        )
        .await
        .expect("client writes the unknown-kind prelude")?;
        let _ = client_send.finish();

        let (server_send, server_recv) = timeout(Duration::from_secs(1), stream_rx.recv())
            .await
            .expect("server accepts the unknown-kind stream")
            .expect("capture handler forwards the unknown-kind stream");

        let supervisor = ZakuraSupervisorHandle::new(16);
        let handler = ZakuraProtocolHandler::new(
            supervisor,
            Network::Mainnet,
            ZakuraHandshakeConfig::for_network(&Network::Mainnet),
            ZakuraLocalLimits::from_config(&Config::default()),
        );

        let peer_id = test_peer(9);
        let stream_sem = Arc::new(Semaphore::new(16));
        // A full bucket with a known capacity so the token charge is observable
        // as an exact decrement.
        let mut open_limiter = TokenBucket::new(4);
        let mut message_buckets = MessageRateBuckets::new();
        let mut workers = JoinSet::new();
        let connection_token = CancellationToken::new();
        let (freshness_tx, _freshness_rx) = watch::channel(Instant::now());

        let mut admission = StreamAdmission {
            trace: handler.trace.clone(),
            conn: ZakuraConnTrace::placeholder(),
            peer_id: &peer_id,
            stream_sem: &stream_sem,
            open_limiter: &mut open_limiter,
            message_buckets: &mut message_buckets,
            workers: &mut workers,
            limits: test_connection_limits(),
            accepted_capabilities: 0,
            connection_token: connection_token.clone(),
            freshness_tx,
        };

        let admitted = handler
            .admit_bi_stream(server_send, server_recv, &mut admission, 16)
            .await;

        assert!(
            admitted.is_none(),
            "an unknown-kind stream must be rejected, not admitted"
        );
        assert!(
            !connection_token.is_cancelled(),
            "an unknown-kind stream is reset stream-only and must keep the connection alive"
        );
        assert_eq!(
            admission.open_limiter.tokens, 3,
            "the protocol-invalid stream open must spend exactly one open-rate token \
             (capacity 4 -> 3); before the fix the unknown-kind reject returned before \
             reaching the limiter, leaving the bucket full at 4"
        );

        client.close().await;
        router.shutdown().await?;
        Ok(())
    }

    #[test]
    fn local_application_frame_cap_admits_default_header_sync_response() {
        let limits = ZakuraLocalLimits::from_config(&Config::default());
        let default_header_sync_frame_bytes =
            u32::try_from(MAX_HS_MESSAGE_BYTES.saturating_add(FRAME_HEADER_BYTES))
                .expect("header-sync frame cap fits in u32");

        assert!(limits.max_frame_bytes >= default_header_sync_frame_bytes);
        assert!(limits.initial_limits().max_frame_bytes >= default_header_sync_frame_bytes);
    }

    #[test]
    fn ordered_stream_stopped_write_is_stream_local() {
        let error: BoxError = iroh::endpoint::WriteError::Stopped(VarInt::from_u32(0)).into();

        assert!(ordered_stream_write_was_stopped(&error));
    }

    #[test]
    fn stream_specific_application_frame_caps_keep_gossip_and_discovery_tight() {
        let limits = ZakuraLocalLimits::from_config(&Config::default());
        let negotiated = limits.clamp(&ZakuraAcceptedLimits {
            max_frame_bytes: u32::MAX,
            max_message_bytes: u32::MAX,
            max_open_streams: u16::MAX,
            max_inbound_queue_depth: u16::MAX,
            idle_timeout_millis: u32::MAX,
        });
        let header_sync_frame_bytes =
            u32::try_from(MAX_HS_MESSAGE_BYTES.saturating_add(FRAME_HEADER_BYTES))
                .expect("header-sync frame cap fits in u32");

        assert_eq!(
            app_frame_cap_for_stream_kind(&negotiated, LEGACY_GOSSIP_STREAM_KIND),
            LOCAL_MAX_CONTROL_FRAME_BYTES
        );
        assert_eq!(
            app_frame_cap_for_stream_kind(&negotiated, LEGACY_REQUEST_STREAM_KIND),
            LOCAL_MAX_CONTROL_FRAME_BYTES
        );
        assert_eq!(
            app_frame_cap_for_stream_kind(&negotiated, DISCOVERY_STREAM_KIND),
            LOCAL_MAX_CONTROL_FRAME_BYTES
        );
        assert_eq!(
            app_frame_cap_for_stream_kind(&negotiated, HEADER_SYNC_STREAM_KIND),
            header_sync_frame_bytes
        );

        let over_tight_cap = usize::try_from(LOCAL_MAX_CONTROL_FRAME_BYTES).unwrap() + 1;
        let header_sync_cap = usize::try_from(header_sync_frame_bytes).unwrap();
        let gossip_frame = Frame {
            message_type: 1,
            flags: 0,
            payload: vec![0; over_tight_cap.saturating_sub(FRAME_HEADER_BYTES)],
        };
        let header_sync_frame = Frame {
            message_type: 1,
            flags: 0,
            payload: vec![0; header_sync_cap.saturating_sub(FRAME_HEADER_BYTES)],
        };

        assert!(
            gossip_frame
                .encode(app_frame_cap_for_stream_kind(
                    &negotiated,
                    LEGACY_GOSSIP_STREAM_KIND
                ))
                .is_err(),
            "gossip frames over the tight stream cap must be rejected"
        );
        assert!(
            gossip_frame
                .encode(app_frame_cap_for_stream_kind(
                    &negotiated,
                    DISCOVERY_STREAM_KIND
                ))
                .is_err(),
            "discovery frames over the tight stream cap must be rejected"
        );
        assert!(
            gossip_frame
                .encode(app_frame_cap_for_stream_kind(
                    &negotiated,
                    LEGACY_REQUEST_STREAM_KIND
                ))
                .is_err(),
            "legacy request frames over the tight stream cap must be rejected"
        );
        assert!(
            header_sync_frame
                .encode(app_frame_cap_for_stream_kind(
                    &negotiated,
                    HEADER_SYNC_STREAM_KIND
                ))
                .is_ok(),
            "header-sync frames up to MAX_HS_MESSAGE_BYTES must still be accepted"
        );
    }

    #[test]
    fn token_bucket_rejects_churn_until_refill() {
        let clock = crate::zakura::testkit::TestClock::new();
        let mut bucket = TokenBucket::with_clock(2, clock.clone());

        assert!(bucket.try_take());
        assert!(bucket.try_take());
        assert!(!bucket.try_take());
        clock.advance(Duration::from_millis(500));
        assert!(bucket.try_take());
        assert!(!bucket.try_take());
        clock.advance(Duration::from_millis(500));
        assert!(bucket.try_take());
    }

    #[test]
    fn supported_stream_accepts_registered_kinds_at_declared_version_only() {
        let registry = ServiceRegistry::new(vec![Arc::new(DeclaredStreamService {
            streams: vec![
                Stream {
                    kind: LEGACY_GOSSIP_STREAM_KIND,
                    version: ZAKURA_STREAM_VERSION_1,
                    frame_cap: 1024,
                    capability: ZAKURA_CAP_LEGACY_GOSSIP,
                    mode: StreamMode::Ordered,
                },
                Stream {
                    kind: LEGACY_REQUEST_STREAM_KIND,
                    version: ZAKURA_STREAM_VERSION_1,
                    frame_cap: 1024,
                    capability: ZAKURA_CAP_LEGACY_GOSSIP,
                    mode: StreamMode::RequestResponse,
                },
                Stream {
                    kind: DISCOVERY_STREAM_KIND,
                    version: ZAKURA_STREAM_VERSION_1,
                    frame_cap: 1024,
                    capability: ZAKURA_CAP_DISCOVERY,
                    mode: StreamMode::Ordered,
                },
                Stream {
                    kind: HEADER_SYNC_STREAM_KIND,
                    version: ZAKURA_STREAM_VERSION_2,
                    frame_cap: 1024,
                    capability: ZAKURA_CAP_HEADER_SYNC,
                    mode: StreamMode::Ordered,
                },
                Stream {
                    kind: ZAKURA_STREAM_BLOCK_SYNC,
                    version: ZAKURA_STREAM_VERSION_1,
                    frame_cap: MAX_BS_FRAME_BYTES,
                    capability: crate::zakura::ZAKURA_CAP_BLOCK_SYNC,
                    mode: StreamMode::Ordered,
                },
            ],
        }) as Arc<dyn Service>])
        .expect("test registry declares unique stream kinds");

        for (kind, version) in [
            (LEGACY_GOSSIP_STREAM_KIND, ZAKURA_STREAM_VERSION_1),
            (LEGACY_REQUEST_STREAM_KIND, ZAKURA_STREAM_VERSION_1),
            (DISCOVERY_STREAM_KIND, ZAKURA_STREAM_VERSION_1),
            (HEADER_SYNC_STREAM_KIND, ZAKURA_STREAM_VERSION_2),
            (ZAKURA_STREAM_BLOCK_SYNC, ZAKURA_STREAM_VERSION_1),
        ] {
            assert!(
                is_supported_stream(&registry, kind, version),
                "registered kind {kind} at declared version {version} must be supported"
            );
            assert!(
                !is_supported_stream(&registry, kind, 0),
                "registered kind {kind} at version 0 must be rejected"
            );
            assert!(
                !is_supported_stream(&registry, kind, version.saturating_add(1)),
                "registered kind {kind} at an unsupported version must be rejected"
            );
        }

        assert!(
            !is_supported_stream(&registry, HEADER_SYNC_STREAM_KIND, ZAKURA_STREAM_VERSION_1),
            "header-sync v1 is intentionally rejected after the clean v2 break"
        );

        assert_eq!(stream_kind_label(2), "gossip");
        assert_eq!(stream_kind_label(3), "legacy_request");
        assert_eq!(stream_kind_label(4), "discovery");
        assert_eq!(stream_kind_label(5), "header_sync");
        assert_eq!(stream_kind_label(6), "block_sync");

        for kind in [0u16, 1, 7, 255, u16::MAX] {
            assert!(
                !is_supported_stream(&registry, kind, ZAKURA_STREAM_VERSION_1),
                "unknown kind {kind} must be rejected even at version 1"
            );
        }

        for kind in [7u16, 255, u16::MAX] {
            assert_eq!(stream_kind_label(kind), "unknown");
        }
    }

    #[test]
    fn legacy_transport_response_validator_accepts_codec_frames() -> Result<(), BoxError> {
        let block = Arc::new(Block::zcash_deserialize(
            BLOCK_TESTNET_141042_BYTES.as_slice(),
        )?);
        let header = block::CountedHeader {
            header: block.header.clone(),
        };
        let tx_id = legacy_tx_id(7);

        assert_codec_frames_validate_at_transport(
            LegacyRequestFrame::BlocksByHash(vec![block.hash()]),
            LegacyRequestKind::Blocks,
            Response::Blocks(vec![InventoryResponse::Available((block, None))]),
        )?;
        assert_codec_frames_validate_at_transport(
            LegacyRequestFrame::TransactionsById(vec![tx_id]),
            LegacyRequestKind::Transactions,
            Response::Transactions(vec![InventoryResponse::Missing(tx_id)]),
        )?;
        assert_codec_frames_validate_at_transport(
            LegacyRequestFrame::FindBlocks {
                known_blocks: vec![block_hash(1)],
                stop: None,
            },
            LegacyRequestKind::FindBlocks,
            Response::BlockHashes(vec![block_hash(2)]),
        )?;
        assert_codec_frames_validate_at_transport(
            LegacyRequestFrame::FindHeaders {
                known_blocks: vec![block_hash(1)],
                stop: None,
            },
            LegacyRequestKind::FindHeaders,
            Response::BlockHeaders(vec![header]),
        )?;
        assert_codec_frames_validate_at_transport(
            LegacyRequestFrame::MempoolTransactionIds,
            LegacyRequestKind::MempoolTransactionIds,
            Response::TransactionIds(vec![tx_id]),
        )?;
        assert_codec_frames_validate_at_transport(
            LegacyRequestFrame::Ping,
            LegacyRequestKind::Ping,
            Response::Pong(Duration::ZERO),
        )?;
        assert_codec_frames_validate_at_transport(
            LegacyRequestFrame::PushTransaction(empty_v5_transaction(8).into()),
            LegacyRequestKind::PushTransaction,
            Response::Nil,
        )?;

        // The inbound service answers an empty FindBlocks/FindHeaders/
        // MempoolTransactionIds with `Response::Nil`, so a lone nil frame must
        // round-trip (encode -> transport validate -> decode) for those data
        // request kinds too, not only PushTransaction.
        assert_codec_frames_validate_at_transport(
            LegacyRequestFrame::FindBlocks {
                known_blocks: vec![block_hash(1)],
                stop: None,
            },
            LegacyRequestKind::FindBlocks,
            Response::Nil,
        )?;
        assert_codec_frames_validate_at_transport(
            LegacyRequestFrame::FindHeaders {
                known_blocks: vec![block_hash(1)],
                stop: None,
            },
            LegacyRequestKind::FindHeaders,
            Response::Nil,
        )?;
        assert_codec_frames_validate_at_transport(
            LegacyRequestFrame::MempoolTransactionIds,
            LegacyRequestKind::MempoolTransactionIds,
            Response::Nil,
        )?;

        Ok(())
    }

    fn assert_codec_frames_validate_at_transport(
        request: LegacyRequestFrame,
        request_kind: LegacyRequestKind,
        response: Response,
    ) -> Result<(), BoxError> {
        let limits = test_connection_limits();
        let request_id = 99;
        let request_frame = request.encode_frame()?;
        let budget = LegacyResponseBudget::from_request(
            request_frame.message_type,
            &request_frame.payload,
            limits,
        )
        .map_err(|error| -> BoxError { format!("{error:?}").into() })?;
        let frames = LegacyResponseCodec::encode_response(
            request_id,
            response,
            limits.max_frame_bytes,
            limits.max_message_bytes,
        )?;
        LegacyResponseCodec::decode_response(request_id, request_kind, frames.clone(), None)?;

        let mut state = LegacyResponseReadState::new(budget);
        for frame in &frames {
            state
                .validate_frame(request_id, frame)
                .map_err(|error| -> BoxError { format!("{error:?}").into() })?;
        }
        state
            .finish()
            .map_err(|error| -> BoxError { format!("{error:?}").into() })?;

        Ok(())
    }

    fn test_connection_limits() -> ZakuraConnectionLimits {
        let max_protocol_message_len =
            u32::try_from(MAX_PROTOCOL_MESSAGE_LEN).expect("protocol message length fits in u32");

        ZakuraConnectionLimits {
            max_frame_bytes: max_protocol_message_len,
            max_message_bytes: max_protocol_message_len,
            max_open_streams: 16,
            max_inbound_queue_depth: 16,
            idle_timeout: Duration::from_secs(1),
            prelude_timeout: Duration::from_secs(1),
            control_timeout: Duration::from_secs(1),
            stream_open_rate_per_second: 16,
            message_rate_per_second: 128,
        }
    }

    fn block_hash(byte: u8) -> block::Hash {
        block::Hash([byte; 32])
    }

    fn legacy_tx_id(byte: u8) -> UnminedTxId {
        UnminedTxId::from_legacy_id(transaction::Hash([byte; 32]))
    }

    fn empty_v5_transaction(byte: u8) -> transaction::Transaction {
        transaction::Transaction::V5 {
            network_upgrade: zebra_chain::parameters::NetworkUpgrade::Nu5,
            lock_time: transaction::LockTime::min_lock_time_timestamp(),
            expiry_height: block::Height(u32::from(byte)),
            inputs: Vec::new(),
            outputs: Vec::new(),
            sapling_shielded_data: None,
            orchard_shielded_data: None,
        }
    }

    #[test]
    fn same_kind_streams_share_one_connection_message_budget() {
        // FLUP-014: two workers serving the SAME kind on ONE connection must
        // draw from a single shared bucket, so opening a second stream does not
        // hand the peer a fresh full budget. Driven by TestClock so the aggregate
        // is asserted on return values, not timing or metrics.
        let clock = crate::zakura::testkit::TestClock::new();
        let mut buckets: MessageRateBuckets<crate::zakura::testkit::TestClock> =
            MessageRateBuckets::new();
        let rate = 4;

        // Two streams of the same kind get clones of the SAME bucket.
        let stream_a = message_bucket_for(&mut buckets, 1, rate, clock.clone());
        let stream_b = message_bucket_for(&mut buckets, 1, rate, clock.clone());
        assert!(
            Arc::ptr_eq(&stream_a, &stream_b),
            "same kind must reuse one shared bucket"
        );
        assert_eq!(
            buckets.len(),
            1,
            "no extra bucket created for the same kind"
        );

        // The aggregate across both handles is capped at the single-bucket budget.
        let take = |bucket: &SharedMessageBucket<crate::zakura::testkit::TestClock>| {
            bucket
                .lock()
                .expect("test bucket mutex is never poisoned")
                .try_take()
        };
        let mut accepted = 0;
        for _ in 0..rate as usize * 4 {
            if take(&stream_a) {
                accepted += 1;
            }
            if take(&stream_b) {
                accepted += 1;
            }
        }
        assert_eq!(
            accepted, rate as usize,
            "the second same-kind stream must NOT get a fresh full budget"
        );

        // After a full refill window the shared budget recovers to one capacity,
        // still shared across both streams (not doubled).
        clock.advance(Duration::from_secs(1));
        let mut refilled = 0;
        for _ in 0..rate as usize * 4 {
            if take(&stream_a) {
                refilled += 1;
            }
            if take(&stream_b) {
                refilled += 1;
            }
        }
        assert_eq!(
            refilled, rate as usize,
            "refill restores one shared budget, not one-per-stream"
        );
    }

    #[test]
    fn different_kinds_get_independent_message_budgets() {
        // FLUP-014: the bucket is keyed per stream-kind, so distinct known kinds
        // do not contend for the same budget.
        let clock = crate::zakura::testkit::TestClock::new();
        let mut buckets: MessageRateBuckets<crate::zakura::testkit::TestClock> =
            MessageRateBuckets::new();

        let request = message_bucket_for(&mut buckets, 1, 2, clock.clone());
        let gossip = message_bucket_for(&mut buckets, 2, 2, clock.clone());
        assert!(
            !Arc::ptr_eq(&request, &gossip),
            "distinct kinds must not share a bucket"
        );
        assert_eq!(buckets.len(), 2);

        let take = |bucket: &SharedMessageBucket<crate::zakura::testkit::TestClock>| {
            bucket
                .lock()
                .expect("test bucket mutex is never poisoned")
                .try_take()
        };
        // Drain the request budget; gossip must be untouched.
        assert!(take(&request));
        assert!(take(&request));
        assert!(!take(&request));
        assert!(take(&gossip));
        assert!(take(&gossip));
        assert!(!take(&gossip));
    }

    #[test]
    fn idle_invariant_keeps_app_timeout_below_quic_timeout() {
        let limits = ZakuraLocalLimits::from_config(&Config::default());

        validate_idle_invariant(&limits).expect("default Zakura limits satisfy idle invariant");
        assert!(
            (limits.initial_limits().idle_timeout_millis as u128)
                < limits.quic_idle_timeout.as_millis()
        );
    }

    // SECURITY AUDIT (candidate claude-legacy-requester-response-frame-growth /
    // trace-gossip-response-reassembler-frame-vector-growth): SR-4 amplification.
    //
    // `write_outbound_request_frame_inner` accumulates every validated response
    // Frame into a `Vec<Frame>` until the responder closes the stream, then hands
    // the whole vector to `decode_response`. The only thing bounding how much it
    // retains is `LegacyResponseBudget::max_bytes`, which `validate_frame` checks
    // as a cumulative byte budget. For Blocks/Transactions that budget was
    // derived as `item_count * max_message_bytes`, so a request naming the
    // protocol-max inventory count (`MAX_TX_INV_IN_SENT_MESSAGE` = 25_000) handed
    // a hostile responder a ~50 GiB retained-frame budget before any decode.
    //
    // The inbound responder already caps a single response's cumulative payload
    // at `LEGACY_RESPONSE_MAX_AGGREGATE_BYTES` (8 * MAX_PROTOCOL_MESSAGE_LEN), so
    // an honest peer never sends more than that. The requester must clamp its
    // retained-frame budget to the same operational aggregate. This test asserts
    // the budget is bounded regardless of requested item count; it FAILS before
    // the fix (budget ~= 50 GiB) and passes after. Do not weaken it to pass.
    #[test]
    fn requester_response_budget_is_capped_for_large_inventory_request() {
        let limits = test_connection_limits();
        assert_eq!(
            limits.max_message_bytes as usize, MAX_PROTOCOL_MESSAGE_LEN,
            "fixture should negotiate the protocol-max message cap so the unclamped \
             budget is maximal",
        );

        let max_items =
            usize::try_from(MAX_TX_INV_IN_SENT_MESSAGE).expect("inventory cap fits in usize");

        // A BlocksByHash request naming the protocol-max inventory count must not
        // grant a retained-frame byte budget above the operational aggregate cap.
        let blocks = LegacyRequestFrame::BlocksByHash(vec![block_hash(7); max_items])
            .encode_frame()
            .expect("max-inventory blocks request encodes");
        let blocks_budget =
            LegacyResponseBudget::from_request(blocks.message_type, &blocks.payload, limits)
                .expect("budget derives from a max-inventory blocks request");
        assert!(
            blocks_budget.max_bytes <= LEGACY_RESPONSE_MAX_AGGREGATE_BYTES,
            "BlocksByHash retained-frame budget {} must be clamped to the aggregate cap {}",
            blocks_budget.max_bytes,
            LEGACY_RESPONSE_MAX_AGGREGATE_BYTES,
        );

        // The transaction-fetch path scales identically and must be clamped too.
        let txs = LegacyRequestFrame::TransactionsById(vec![legacy_tx_id(7); max_items])
            .encode_frame()
            .expect("max-inventory transactions request encodes");
        let txs_budget = LegacyResponseBudget::from_request(txs.message_type, &txs.payload, limits)
            .expect("budget derives from a max-inventory transactions request");
        assert!(
            txs_budget.max_bytes <= LEGACY_RESPONSE_MAX_AGGREGATE_BYTES,
            "TransactionsById retained-frame budget {} must be clamped to the aggregate cap {}",
            txs_budget.max_bytes,
            LEGACY_RESPONSE_MAX_AGGREGATE_BYTES,
        );

        // The clamp must only remove the unbounded tail: a modest request still
        // has to accept at least one full negotiated message, otherwise we would
        // wrongly reject honest single-item responses.
        let small = LegacyRequestFrame::BlocksByHash(vec![block_hash(1)])
            .encode_frame()
            .expect("single-item blocks request encodes");
        let small_budget =
            LegacyResponseBudget::from_request(small.message_type, &small.payload, limits)
                .expect("budget derives from a single-item blocks request");
        assert!(
            small_budget.max_bytes >= limits.max_message_bytes as usize,
            "a single-item request must still permit one full response message; budget was {}",
            small_budget.max_bytes,
        );
        assert!(
            small_budget.max_bytes <= LEGACY_RESPONSE_MAX_AGGREGATE_BYTES,
            "even a single-item budget stays within the aggregate cap",
        );
    }

    // SECURITY AUDIT (candidate claude-legacy-nil-response-nonfatal /
    // subset-response-correlation-gossip-nil-response-nonfatal): SR-7 fail-closed.
    //
    // The outbound request stream worker decides connection-fatality from
    // `LegacyResponseReadState::validate_frame`: `Fatal` => connection.close() +
    // connection_token.cancel() (the peer is disconnected); `Ok`/`Local` => the
    // peer stays connected and the request just returns an error. `validate_nil`
    // accepts a `MSG_RESPONSE_NIL` sentinel for *every* request kind, so a peer
    // that answers an inventory fetch (BlocksByHash / TransactionsById) or a Ping
    // with a correct-id NIL passes the transport budget layer (worker returns
    // Ok(frames)) and is NOT disconnected. Only the later `decode_response` layer
    // rejects NIL for these kinds -- as an ordinary request-local error. The two
    // layers disagree, so an unexpected/unsolicited response is tolerated instead
    // of failing closed.
    //
    // This test asserts the SAFE behavior (the transport budget layer must reject
    // NIL for inventory/Ping kinds as `Fatal`, so the worker disconnects). It
    // currently FAILS, which is the reproduction. Do not weaken it to pass.
    #[test]
    fn nil_response_to_inventory_or_ping_request_is_not_fail_closed() {
        let limits = test_connection_limits();
        let request_id = 99;

        let cases: [(LegacyRequestFrame, LegacyRequestKind); 3] = [
            (
                LegacyRequestFrame::BlocksByHash(vec![block_hash(1)]),
                LegacyRequestKind::Blocks,
            ),
            (
                LegacyRequestFrame::TransactionsById(vec![legacy_tx_id(2)]),
                LegacyRequestKind::Transactions,
            ),
            (LegacyRequestFrame::Ping, LegacyRequestKind::Ping),
        ];

        for (request, request_kind) in cases {
            let request_frame = request.encode_frame().expect("request frame encodes");
            let budget = LegacyResponseBudget::from_request(
                request_frame.message_type,
                &request_frame.payload,
                limits,
            )
            .expect("budget derives from request");

            // A hostile/buggy responder serializes Response::Nil with the real
            // codec, addressed to our request id.
            let nil_frames = LegacyResponseCodec::encode_response(
                request_id,
                Response::Nil,
                limits.max_frame_bytes,
                limits.max_message_bytes,
            )
            .expect("nil response encodes");

            // The higher decode layer DOES reject NIL for these kinds...
            let decoded = LegacyResponseCodec::decode_response(
                request_id,
                request_kind,
                nil_frames.clone(),
                None,
            );
            assert!(
                decoded.is_err(),
                "decode_response must reject a bare NIL for {request_kind:?}",
            );

            // ...but the transport budget layer -- the one that drives the
            // fail-closed disconnect in the request stream worker -- must ALSO
            // reject it as Fatal. It currently accepts it.
            let mut state = LegacyResponseReadState::new(budget);
            let mut validate = Ok(());
            for frame in &nil_frames {
                validate = state.validate_frame(request_id, frame);
                if validate.is_err() {
                    break;
                }
            }
            let validate = validate.and_then(|()| state.finish());
            assert!(
                matches!(validate, Err(OutboundRequestError::Fatal(_))),
                "transport must fail closed (Fatal) on a NIL answer to {request_kind:?} so the \
                 request stream worker disconnects the peer; got {validate:?}",
            );
        }
    }

    // SECURITY AUDIT (candidate claude-inbound-per-ip-cap-bypassed /
    // codex-inbound-per-ip-cap-bypass): SR-4 admission.
    //
    // `accept_connection` used to register every inbound peer with
    // `remote_ip = None`, and `register` only consults `active_by_ip` when
    // `remote_ip` is `Some`, so the per-IP connection cap was enforced for native
    // outbound dials (which pass a real IP) but entirely bypassed for inbound
    // Router accepts: one source IP could authenticate as many distinct iroh node
    // ids and fill the global connection budget despite `max_connections_per_ip`
    // (default 1). The fix resolves the inbound peer's UDP source IP from the
    // endpoint's node map at the accept site and passes it into `register`, so the
    // per-IP cap now applies to Router-accepted connections too.
    //
    // This guard drives the real production `ProtocolHandler::accept` over a
    // loopback iroh transport: two distinct authenticated identities dial from the
    // same source IP (127.0.0.1) through the full native handshake. The per-IP cap
    // is 1 while global admission keeps its default (well above 1), so the second
    // identity can only be turned away by the per-IP cap, not the global gate.
    // Before the fix both identities registered; now the second is rejected and
    // its connection is closed, leaving exactly one registered peer.
    #[tokio::test]
    async fn inbound_accept_enforces_per_ip_cap() -> Result<(), BoxError> {
        let _guard = zebra_test::init();

        // The register-level invariant the accept path now relies on: with a real
        // source IP, the per-IP cap rejects a second distinct identity with
        // `ResourceLimit`.
        async fn try_register(
            supervisor: &ZakuraSupervisorHandle,
            peer: &ZakuraPeerId,
            remote_ip: Option<IpAddr>,
        ) -> ZakuraRegistration {
            let (outbound_tx, _outbound_rx) = mpsc::channel(1);
            let outbound_handle = ZakuraPeerHandle::new_for_tests(peer.clone(), outbound_tx);
            supervisor
                .register(
                    peer.clone(),
                    remote_ip,
                    [peer.as_bytes()[0]; TRANSCRIPT_HASH_BYTES],
                    outbound_handle,
                    CancellationToken::new(),
                    ZAKURA_CAP_LEGACY_GOSSIP | ZAKURA_CAP_HEADER_SYNC,
                )
                .await
        }
        let ip: IpAddr = "203.0.113.7".parse().expect("test ip parses");
        let supervisor = ZakuraSupervisorHandle::new(1);
        assert!(
            matches!(
                try_register(&supervisor, &test_peer(1), Some(ip)).await,
                ZakuraRegistration::Registered { .. }
            ),
            "first identity from the IP registers",
        );
        assert!(
            matches!(
                try_register(&supervisor, &test_peer(2), Some(ip)).await,
                ZakuraRegistration::Rejected(ZakuraRejectReason::ResourceLimit)
            ),
            "a second distinct identity from the same IP must be Rejected(ResourceLimit) at cap 1",
        );

        // End-to-end: drive the production accept path with a per-IP cap of 1 and a
        // strictly larger global cap so per-IP admission is what turns away the
        // second same-IP identity. Wire the bound endpoint so the accept path can
        // resolve the inbound source IP.
        let limits = ZakuraLocalLimits::from_config(&Config::default());
        assert!(
            limits.max_connections > 1,
            "global admission cap must exceed the per-IP cap so the second same-IP identity is \
             turned away by the per-IP cap rather than the global gate",
        );
        let server_ep = LocalEndpointFactory::with_transport_config(limits.transport_config())
            .endpoint(880)
            .await?;
        let supervisor = ZakuraSupervisorHandle::new(1);
        let handler = ZakuraProtocolHandler::new(
            supervisor.clone(),
            Network::Mainnet,
            ZakuraHandshakeConfig::for_network(&Network::Mainnet),
            limits.clone(),
        )
        .with_endpoint(server_ep.clone());
        let router = Router::builder(server_ep)
            .accept(P2P_V2_ALPN, handler)
            .spawn();
        // Iroh also binds a default IPv6 socket, so an unrestricted node address
        // lets the two clients reach the server over different paths (e.g. one
        // IPv4 loopback, one global IPv6) and therefore present different source
        // IPs. Pin both dials to the server's IPv4 loopback address so they share
        // one source IP (127.0.0.1) -- the single-source-IP shape of the finding.
        let full_addr = router.endpoint().node_addr().initialized().await;
        let loopback_addr = NodeAddr::new(full_addr.node_id).with_direct_addresses(
            full_addr
                .direct_addresses()
                .copied()
                .filter(|addr| addr.is_ipv4() && addr.ip().is_loopback()),
        );
        assert!(
            loopback_addr.direct_addresses().next().is_some(),
            "server must advertise an IPv4 loopback direct address",
        );
        let server_addr = loopback_addr;

        // Establish the QUIC connection only; the native handshake is driven
        // separately so a per-IP rejection mid-handshake (the second identity)
        // can be observed instead of aborting the test.
        async fn connect_native(
            server_addr: &NodeAddr,
            seed: u64,
            limits: &ZakuraLocalLimits,
        ) -> Result<(Endpoint, Connection), BoxError> {
            let endpoint = LocalEndpointFactory::with_transport_config(limits.transport_config())
                .endpoint(seed)
                .await?;
            endpoint.add_node_addr(server_addr.clone())?;
            let connection = endpoint.connect(server_addr.clone(), P2P_V2_ALPN).await?;
            Ok((endpoint, connection))
        }
        async fn run_handshake(
            endpoint: &Endpoint,
            connection: &Connection,
            limits: &ZakuraLocalLimits,
        ) -> Result<(), BoxError> {
            let config = ZakuraHandshakeConfig::for_network(&Config::default().network);
            let local_peer_id = ZakuraPeerId::new(endpoint.node_id().as_bytes().to_vec())?;
            run_native_initiator_handshake_without_trace(
                connection,
                limits,
                &config,
                &local_peer_id,
            )
            .await?;
            Ok(())
        }

        // First identity registers and claims the only per-IP slot. Hold the
        // endpoint/connection so the peer stays registered for the second dial.
        let (_ep1, _conn1) = connect_native(&server_addr, 881, &limits).await?;
        run_handshake(&_ep1, &_conn1, &limits).await?;
        let mut first_registered = 0;
        for _ in 0..200 {
            first_registered = supervisor.registered_ids().await.len();
            if first_registered >= 1 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        assert_eq!(
            first_registered, 1,
            "the first inbound identity from the source IP must register (a real source IP was \
             resolved from the endpoint and counted against the per-IP cap)",
        );

        // Second distinct identity from the same source IP: the per-IP cap must
        // reject its registration. The handshake may complete and then be closed,
        // or be torn down mid-handshake by the rejection -- either way the server
        // closes the connection with the resource-limit code and never registers a
        // second peer. (Before the fix the accept passed remote_ip = None, so this
        // identity registered and one source IP could exhaust the global budget.)
        let (_ep2, conn2) = connect_native(&server_addr, 882, &limits).await?;
        let _ = run_handshake(&_ep2, &conn2, &limits).await;
        let mut rejected_close = false;
        for _ in 0..400 {
            if supervisor.registered_ids().await.len() >= 2 {
                break;
            }
            if matches!(
                conn2.close_reason(),
                Some(iroh::endpoint::ConnectionError::ApplicationClosed(ref close))
                    if close.error_code == VarInt::from_u32(ZAKURA_CLOSE_RESOURCE)
            ) {
                rejected_close = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        let registered = supervisor.registered_ids().await.len();
        assert!(
            rejected_close && registered == 1,
            "a second distinct identity from the same source IP must be rejected by the per-IP \
             cap with a resource-limit close (resource_close={rejected_close}, \
             registered={registered}); before the fix the inbound accept passed remote_ip = None, \
             so both identities registered and one source IP could exhaust the connection budget",
        );

        router.shutdown().await?;
        Ok(())
    }
}
