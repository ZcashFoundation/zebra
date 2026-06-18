//! Native Zakura discovery wire messages and signed node records.

use std::{
    cmp::Reverse,
    collections::{HashMap, HashSet},
    fmt,
    io::{self, Cursor, Read, Write},
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, SocketAddrV4, SocketAddrV6},
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use byteorder::{LittleEndian, ReadBytesExt, WriteBytesExt};
use iroh::{NodeAddr, NodeId, SecretKey};
use rand::seq::IteratorRandom;
use thiserror::Error;
use tokio::sync::{watch, Mutex};
use zebra_chain::{
    block,
    primitives::ed25519::{Signature, SigningKey, VerificationKey},
};

use crate::zakura::{
    BlockSyncStatus, ServiceAdmissionDecision, ServicePeerDirection, ServicePeerLimits,
    ServicePeerSnapshot, ZakuraNetworkId, ZakuraPeerId, MAX_BS_BLOCKS_PER_REQUEST,
    MAX_BS_RESPONSE_BYTES,
};

/// Native discovery stream kind.
pub const ZAKURA_STREAM_DISCOVERY: u16 = 4;
/// Native discovery stream version.
pub const ZAKURA_DISCOVERY_STREAM_VERSION: u16 = 1;

/// Discovery hello message carrying the sender's signed record.
pub const MSG_DISCOVERY_HELLO: u8 = 1;
/// Discovery peer sample request.
pub const MSG_DISCOVERY_GET_PEERS: u8 = 2;
/// Discovery peer sample response.
pub const MSG_DISCOVERY_PEERS: u8 = 3;
/// First-party live service summary request.
pub const MSG_DISCOVERY_GET_SERVICES: u8 = 4;
/// First-party live service summary response.
pub const MSG_DISCOVERY_SERVICES: u8 = 5;

/// Maximum bytes in an encoded discovery message.
pub const MAX_DISCOVERY_MESSAGE_BYTES: usize = 16 * 1024;
/// Maximum bytes in an encoded node record body.
pub const MAX_NODE_RECORD_BODY_BYTES: usize = 16 * 1024;
/// Maximum direct addresses in a node record.
pub const MAX_DIRECT_ADDRS_PER_RECORD: usize = 8;
/// Maximum services in a node record or query.
pub const MAX_SERVICES_PER_RECORD: usize = 32;
/// Maximum services requested in a first-party live-service query.
pub const MAX_GET_SERVICES_WANTED: usize = 32;
/// Maximum first-party live summaries in a service response.
pub const MAX_SERVICE_SUMMARIES_PER_RESPONSE: usize = 32;
/// Maximum bytes in one length-delimited live summary payload.
pub const MAX_SERVICE_SUMMARY_BYTES: usize = 1024;
/// Maximum records in a discovery response.
pub const MAX_DISCOVERY_RECORDS_PER_RESPONSE: usize = 32;
/// Maximum excluded node ids in a discovery query.
pub const MAX_DISCOVERY_EXCLUDED_NODE_IDS: usize = 256;
/// Maximum bytes in a service id.
pub const MAX_ZAKURA_SERVICE_ID_BYTES: usize = 64;
/// Default lifetime for locally authored discovery records.
pub const DEFAULT_DISCOVERY_RECORD_TTL: Duration = Duration::from_secs(24 * 60 * 60);
/// Default minimum interval between active discovery refreshes.
pub const DEFAULT_DISCOVERY_REFRESH_INTERVAL: Duration = Duration::from_secs(10 * 60);
/// Default peer sample size requested by active discovery.
pub const DEFAULT_DISCOVERY_PEER_SAMPLE_LIMIT: usize = 32;
/// Default maximum accepted future TTL on imported records.
pub const DEFAULT_DISCOVERY_MAX_RECORD_TTL: Duration = Duration::from_secs(24 * 60 * 60);
/// Default expiry clock-skew tolerance for imported records.
pub const DEFAULT_DISCOVERY_CLOCK_SKEW_TOLERANCE: Duration = Duration::from_secs(5 * 60);
/// Default base backoff for discovery dials.
pub const DEFAULT_DISCOVERY_DIAL_BACKOFF_BASE: Duration = Duration::from_secs(60);
/// Default maximum backoff for discovery dials.
pub const DEFAULT_DISCOVERY_DIAL_BACKOFF_MAX: Duration = Duration::from_secs(60 * 60);
/// Default concurrent native discovery dial cap.
pub const DEFAULT_MAX_CONCURRENT_DISCOVERY_DIALS: usize = 4;
/// Default connection slots kept out of discovery dialing.
pub const DEFAULT_DISCOVERY_CONNECTION_HEADROOM: usize = 4;
/// Default Zakura connection cap used by standalone discovery-state tests.
pub const DEFAULT_DISCOVERY_ZAKURA_MAX_CONNECTIONS: usize = 32;
/// Default lifetime for first-party live service summaries.
pub const DEFAULT_LIVE_SERVICE_SUMMARY_TTL: Duration = Duration::from_secs(30);

/// Native peer discovery service id.
pub const SERVICE_ID_DISCOVERY: &str = "zakura.discovery.v1";
/// Native header-sync service id.
pub const SERVICE_ID_HEADER_SYNC: &str = "zakura.header_sync.v1";
/// Native block-sync service id.
pub const SERVICE_ID_BLOCK_SYNC: &str = "zakura.block_sync.v1";
/// Native legacy gossip service id.
pub const SERVICE_ID_LEGACY_GOSSIP: &str = "zakura.legacy_gossip.v1";
/// Native legacy requests service id.
pub const SERVICE_ID_LEGACY_REQUESTS: &str = "zakura.legacy_requests.v1";
/// Native service discovery service id.
pub const SERVICE_ID_SERVICE_DISCOVERY: &str = "zakura.service_discovery.v1";
/// Summary tag for [`HeaderSyncServiceSummary`].
pub const SUMMARY_TAG_HEADER_SYNC_V1: u16 = 1;
/// Summary tag for [`DiscoveryServiceSummary`].
pub const SUMMARY_TAG_DISCOVERY_V1: u16 = 2;
/// Summary tag for [`BlockSyncServiceSummary`].
pub const SUMMARY_TAG_BLOCK_SYNC_V1: u16 = 3;

const SIGNATURE_BYTES: usize = 64;
const NODE_ID_BYTES: usize = 32;
const SOCKET_ADDR_V4: u8 = 4;
const SOCKET_ADDR_V6: u8 = 6;
const ZAKURA_NODE_RECORD_SIG_DOMAIN: &[u8] = b"zakura-node-record-v1";
const ZAKURA_NODE_RECORD_FORMAT_VERSION: u16 = ZAKURA_DISCOVERY_STREAM_VERSION;

/// A bounded ASCII Zakura service identifier.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ZakuraServiceId(String);

impl ZakuraServiceId {
    /// Creates a bounded ASCII service id.
    pub fn new(value: impl Into<String>) -> Result<Self, DiscoveryWireError> {
        let value = value.into();
        validate_service_id(value.as_bytes())?;
        Ok(Self(value))
    }

    /// Returns the service id as a string slice.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Returns the native peer discovery service id.
    pub fn discovery() -> Self {
        Self::new(SERVICE_ID_DISCOVERY)
            .expect("built-in Zakura discovery service id is non-empty bounded ASCII")
    }

    /// Returns the native header-sync service id.
    pub fn header_sync() -> Self {
        Self::new(SERVICE_ID_HEADER_SYNC)
            .expect("built-in Zakura header-sync service id is non-empty bounded ASCII")
    }

    /// Returns the native block-sync service id.
    pub fn block_sync() -> Self {
        Self::new(SERVICE_ID_BLOCK_SYNC)
            .expect("built-in Zakura block-sync service id is non-empty bounded ASCII")
    }

    /// Returns the native legacy gossip service id.
    pub fn legacy_gossip() -> Self {
        Self::new(SERVICE_ID_LEGACY_GOSSIP)
            .expect("built-in Zakura legacy gossip service id is non-empty bounded ASCII")
    }

    /// Returns the native legacy requests service id.
    pub fn legacy_requests() -> Self {
        Self::new(SERVICE_ID_LEGACY_REQUESTS)
            .expect("built-in Zakura legacy requests service id is non-empty bounded ASCII")
    }

    /// Returns the native service discovery service id.
    pub fn service_discovery() -> Self {
        Self::new(SERVICE_ID_SERVICE_DISCOVERY)
            .expect("built-in Zakura service discovery id is non-empty bounded ASCII")
    }
}

impl fmt::Display for ZakuraServiceId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl TryFrom<&str> for ZakuraServiceId {
    type Error = DiscoveryWireError;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl TryFrom<String> for ZakuraServiceId {
    type Error = DiscoveryWireError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

/// The signed fields in a Zakura node record.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ZakuraNodeRecordBody {
    /// The authoring iroh node id.
    pub node_id: NodeId,
    /// Direct dial addresses advertised by the author.
    pub direct_addrs: Vec<SocketAddr>,
    /// Native services advertised by the author.
    pub services: Vec<ZakuraServiceId>,
    /// Lowest supported Zakura protocol version.
    pub zakura_protocol_min: u16,
    /// Highest supported Zakura protocol version.
    pub zakura_protocol_max: u16,
    /// Zakura network id.
    pub network_id: ZakuraNetworkId,
    /// Genesis hash / chain id.
    pub chain_id: [u8; 32],
    /// Monotonic author sequence number.
    pub sequence: u64,
    /// Unix timestamp when this record expires.
    pub expires_at_unix_secs: u64,
}

impl ZakuraNodeRecordBody {
    /// Encodes this body into canonical bytes for signing and verification.
    pub fn encode_for_signature(&self) -> Result<Vec<u8>, DiscoveryWireError> {
        validate_record_body_bounds(self)?;

        let mut bytes = Vec::new();
        bytes.write_all(ZAKURA_NODE_RECORD_SIG_DOMAIN)?;
        bytes.write_u16::<LittleEndian>(ZAKURA_NODE_RECORD_FORMAT_VERSION)?;
        encode_record_body_to(self, &mut bytes)?;
        if bytes.len() > MAX_NODE_RECORD_BODY_BYTES {
            return Err(DiscoveryWireError::OversizedPayload {
                actual: bytes.len(),
                max: MAX_NODE_RECORD_BODY_BYTES,
            });
        }
        Ok(bytes)
    }
}

/// A self-signed Zakura node record.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ZakuraNodeRecord {
    /// Signed record body.
    pub body: ZakuraNodeRecordBody,
    /// Ed25519 signature over [`ZakuraNodeRecordBody::encode_for_signature`].
    pub signature: Signature,
}

impl ZakuraNodeRecord {
    /// Signs `body` with the iroh secret key matching `body.node_id`.
    pub fn sign(
        body: ZakuraNodeRecordBody,
        secret_key: &SecretKey,
    ) -> Result<Self, DiscoveryWireError> {
        if body.node_id != secret_key.public() {
            return Err(DiscoveryWireError::SigningKeyMismatch);
        }
        let signing_key = SigningKey::from(secret_key.to_bytes());
        let signature = signing_key.sign(&body.encode_for_signature()?);
        Ok(Self { body, signature })
    }

    /// Verifies this record signature and advisory import context.
    pub fn verify(
        &self,
        context: &DiscoveryRecordValidationContext,
    ) -> Result<(), DiscoveryRecordError> {
        validate_record_body_for_import(&self.body, context)?;

        let verification_key = VerificationKey::try_from(&self.body.node_id.as_bytes()[..])
            .map_err(|_| DiscoveryRecordError::MalformedNodeId)?;
        let body_bytes = self
            .body
            .encode_for_signature()
            .map_err(DiscoveryRecordError::Wire)?;
        verification_key
            .verify(&self.signature, &body_bytes)
            .map_err(|_| DiscoveryRecordError::InvalidSignature)
    }
}

/// Context used when validating a node record for import.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DiscoveryRecordValidationContext {
    /// Expected local network id.
    pub expected_network_id: ZakuraNetworkId,
    /// Expected local chain id.
    pub expected_chain_id: [u8; 32],
    /// Current Unix timestamp.
    pub current_unix_secs: u64,
    /// Lowest locally supported Zakura protocol version.
    pub supported_protocol_min: u16,
    /// Highest locally supported Zakura protocol version.
    pub supported_protocol_max: u16,
    /// Maximum accepted record TTL beyond `current_unix_secs`.
    pub max_record_ttl: Duration,
    /// Accepted wall-clock skew on expiry edges.
    pub clock_skew_tolerance: Duration,
}

/// First-party live service summary request.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GetServices {
    /// Requested service ids, or empty for every locally available summary.
    pub wanted_services: Vec<ZakuraServiceId>,
}

/// First-party live service summary response.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Services {
    /// Node id of the responder that authored these live summaries.
    pub node_id: NodeId,
    /// Unix timestamp after which these live summaries are stale.
    pub expires_at_unix_secs: u64,
    /// Length-delimited live summaries for this responder only.
    pub summaries: Vec<ServiceSummaryEnvelope>,
}

/// Length-delimited live service summary envelope.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ServiceSummaryEnvelope {
    /// Service this summary describes.
    pub service_id: ZakuraServiceId,
    /// Versioned summary tag for the payload.
    pub summary_tag: u16,
    /// Bounded encoded summary payload.
    pub summary_bytes: Vec<u8>,
}

impl ServiceSummaryEnvelope {
    /// Build a header-sync summary envelope.
    pub fn header_sync(summary: &HeaderSyncServiceSummary) -> Result<Self, DiscoveryWireError> {
        let mut summary_bytes = Vec::new();
        encode_header_sync_summary(summary, &mut summary_bytes)?;
        Ok(Self {
            service_id: ZakuraServiceId::header_sync(),
            summary_tag: SUMMARY_TAG_HEADER_SYNC_V1,
            summary_bytes,
        })
    }

    /// Build a discovery summary envelope.
    pub fn discovery(summary: &DiscoveryServiceSummary) -> Result<Self, DiscoveryWireError> {
        let mut summary_bytes = Vec::new();
        encode_discovery_summary(summary, &mut summary_bytes)?;
        Ok(Self {
            service_id: ZakuraServiceId::discovery(),
            summary_tag: SUMMARY_TAG_DISCOVERY_V1,
            summary_bytes,
        })
    }

    /// Build a block-sync summary envelope.
    pub fn block_sync(summary: &BlockSyncServiceSummary) -> Result<Self, DiscoveryWireError> {
        let mut summary_bytes = Vec::new();
        encode_block_sync_summary(summary, &mut summary_bytes)?;
        Ok(Self {
            service_id: ZakuraServiceId::block_sync(),
            summary_tag: SUMMARY_TAG_BLOCK_SYNC_V1,
            summary_bytes,
        })
    }

    /// Decode this envelope as a header-sync summary when its tag matches.
    pub fn decode_header_sync(
        &self,
    ) -> Result<Option<HeaderSyncServiceSummary>, DiscoveryWireError> {
        if self.summary_tag == SUMMARY_TAG_HEADER_SYNC_V1 {
            decode_header_sync_summary(&self.summary_bytes).map(Some)
        } else {
            Ok(None)
        }
    }

    /// Decode this envelope as a discovery summary when its tag matches.
    pub fn decode_discovery(&self) -> Result<Option<DiscoveryServiceSummary>, DiscoveryWireError> {
        if self.summary_tag == SUMMARY_TAG_DISCOVERY_V1 {
            decode_discovery_summary(&self.summary_bytes).map(Some)
        } else {
            Ok(None)
        }
    }

    /// Decode this envelope as a block-sync summary when its tag matches.
    pub fn decode_block_sync(&self) -> Result<Option<BlockSyncServiceSummary>, DiscoveryWireError> {
        if self.summary_tag == SUMMARY_TAG_BLOCK_SYNC_V1 {
            decode_block_sync_summary(&self.summary_bytes).map(Some)
        } else {
            Ok(None)
        }
    }
}

/// Header-sync live summary used only as a dial/admission hint.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct HeaderSyncServiceSummary {
    /// Best known header height.
    pub best_height: block::Height,
    /// Best known header hash.
    pub best_hash: block::Hash,
    /// Finalized height, when cheaply available.
    pub finalized_height: Option<block::Height>,
    /// Whether this node is currently serving headers.
    pub serving_headers: bool,
    /// Free inbound header-sync slots.
    pub inbound_slots_free: u16,
    /// Maximum inbound header-sync slots.
    pub inbound_slots_max: u16,
    /// Free outbound header-sync slots.
    pub outbound_slots_free: u16,
    /// Maximum outbound header-sync slots.
    pub outbound_slots_max: u16,
}

impl HeaderSyncServiceSummary {
    /// Build a summary from the header-sync reactor's cheap local snapshots.
    pub fn from_snapshot(
        best_height: block::Height,
        best_hash: block::Hash,
        finalized_height: Option<block::Height>,
        serving_headers: bool,
        peers: ServicePeerSnapshot,
    ) -> Self {
        Self {
            best_height,
            best_hash,
            finalized_height,
            serving_headers,
            inbound_slots_free: saturating_u16(peers.inbound_slots_free),
            inbound_slots_max: saturating_u16(
                peers.inbound_peers.saturating_add(peers.inbound_slots_free),
            ),
            outbound_slots_free: saturating_u16(peers.outbound_slots_free),
            outbound_slots_max: saturating_u16(
                peers
                    .outbound_peers
                    .saturating_add(peers.outbound_slots_free),
            ),
        }
    }
}

/// Discovery live summary used only as a dial/admission hint.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct DiscoveryServiceSummary {
    /// Free inbound discovery exchange slots.
    pub peer_exchange_slots_free: u16,
    /// Maximum records this node will return in one peer response.
    pub max_records_per_response: u16,
    /// Whether this node expects to disconnect after one exchange.
    pub expected_disconnect_after_exchange: bool,
}

impl DiscoveryServiceSummary {
    /// Build a summary from the discovery runtime's cheap local snapshots.
    pub fn from_snapshot(
        peers: ServicePeerSnapshot,
        max_records_per_response: usize,
        expected_disconnect_after_exchange: bool,
    ) -> Self {
        Self {
            peer_exchange_slots_free: saturating_u16(peers.inbound_slots_free),
            max_records_per_response: saturating_u16(
                max_records_per_response.min(MAX_DISCOVERY_RECORDS_PER_RESPONSE),
            ),
            expected_disconnect_after_exchange,
        }
    }
}

/// Block-sync live summary used only as a dial/admission hint.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct BlockSyncServiceSummary {
    /// Earliest committed body height this node can serve.
    pub servable_low: block::Height,
    /// Highest committed body height this node can serve.
    pub servable_high: block::Height,
    /// Hash of [`servable_high`](Self::servable_high).
    pub tip_hash: block::Hash,
    /// Free serving slots for inbound `GetBlocks` requests.
    pub free_slots: u16,
    /// Maximum blocks this node will return in one block-sync response.
    pub max_blocks_per_response: u32,
    /// Maximum encoded bytes this node will return in one block-sync response.
    pub max_response_bytes: u32,
}

impl BlockSyncServiceSummary {
    /// Build a summary from the block-sync reactor's cheap local status and peer snapshots.
    pub fn from_status_and_snapshot(status: BlockSyncStatus, peers: ServicePeerSnapshot) -> Self {
        Self {
            servable_low: status.servable_low,
            servable_high: status.servable_high,
            tip_hash: status.tip_hash,
            free_slots: saturating_u16(peers.inbound_slots_free),
            max_blocks_per_response: status.max_blocks_per_response,
            max_response_bytes: status.max_response_bytes,
        }
    }

    /// Returns true if this summary can serve every height in `gap`.
    pub fn covers_gap(&self, gap: &[block::Height]) -> bool {
        self.gap_coverage(gap) == BlockSyncGapCoverage::Whole
    }

    /// Returns how much of `gap` this summary can serve.
    fn gap_coverage(&self, gap: &[block::Height]) -> BlockSyncGapCoverage {
        if self.free_slots == 0 {
            return BlockSyncGapCoverage::None;
        }

        let Some(first) = gap.iter().min().copied() else {
            return BlockSyncGapCoverage::None;
        };
        let Some(last) = gap.iter().max().copied() else {
            return BlockSyncGapCoverage::None;
        };
        if self.servable_low <= first && last <= self.servable_high {
            BlockSyncGapCoverage::Whole
        } else if self.servable_low <= first && first <= self.servable_high {
            BlockSyncGapCoverage::LowPrefix
        } else {
            BlockSyncGapCoverage::None
        }
    }
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum BlockSyncGapCoverage {
    None,
    LowPrefix,
    Whole,
}

/// Native discovery protocol messages.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DiscoveryMessage {
    /// Sender's signed self-record.
    Hello {
        /// Signed node record.
        record: ZakuraNodeRecord,
    },
    /// Request a bounded peer sample.
    GetPeers {
        /// Maximum requested records.
        limit: u16,
        /// Optional service filter.
        wanted_services: Vec<ZakuraServiceId>,
        /// Node ids the responder should not return.
        exclude_node_ids: Vec<NodeId>,
    },
    /// Bounded signed peer records.
    Peers {
        /// Signed node records.
        records: Vec<ZakuraNodeRecord>,
    },
    /// Request first-party live service summaries.
    GetServices(GetServices),
    /// First-party live service summaries.
    Services(Services),
}

impl DiscoveryMessage {
    /// Encodes this message with deterministic length-prefixed fields.
    pub fn encode(&self) -> Result<Vec<u8>, DiscoveryWireError> {
        let mut bytes = Vec::new();
        match self {
            Self::Hello { record } => {
                bytes.write_u8(MSG_DISCOVERY_HELLO)?;
                encode_record(record, &mut bytes)?;
            }
            Self::GetPeers {
                limit,
                wanted_services,
                exclude_node_ids,
            } => {
                validate_query_fields(*limit, wanted_services, exclude_node_ids)?;
                bytes.write_u8(MSG_DISCOVERY_GET_PEERS)?;
                encode_query_fields(*limit, wanted_services, exclude_node_ids, &mut bytes)?;
            }
            Self::Peers { records } => {
                encode_records_message(
                    MSG_DISCOVERY_PEERS,
                    records,
                    MAX_DISCOVERY_RECORDS_PER_RESPONSE,
                    &mut bytes,
                )?;
            }
            Self::GetServices(query) => {
                validate_get_services(query)?;
                bytes.write_u8(MSG_DISCOVERY_GET_SERVICES)?;
                encode_service_ids_capped(
                    &query.wanted_services,
                    MAX_GET_SERVICES_WANTED,
                    &mut bytes,
                )?;
            }
            Self::Services(services) => {
                validate_services(services)?;
                bytes.write_u8(MSG_DISCOVERY_SERVICES)?;
                encode_services_message(services, &mut bytes)?;
            }
        }

        if bytes.len() > MAX_DISCOVERY_MESSAGE_BYTES {
            return Err(DiscoveryWireError::OversizedPayload {
                actual: bytes.len(),
                max: MAX_DISCOVERY_MESSAGE_BYTES,
            });
        }

        Ok(bytes)
    }

    /// Decodes a discovery message, rejecting oversize and trailing bytes.
    pub fn decode(bytes: &[u8]) -> Result<Self, DiscoveryWireError> {
        if bytes.len() > MAX_DISCOVERY_MESSAGE_BYTES {
            return Err(DiscoveryWireError::OversizedPayload {
                actual: bytes.len(),
                max: MAX_DISCOVERY_MESSAGE_BYTES,
            });
        }

        let mut reader = Cursor::new(bytes);
        let message = match reader.read_u8()? {
            MSG_DISCOVERY_HELLO => Self::Hello {
                record: decode_record(&mut reader)?,
            },
            MSG_DISCOVERY_GET_PEERS => {
                let (limit, wanted_services, exclude_node_ids) = decode_query_fields(&mut reader)?;
                Self::GetPeers {
                    limit,
                    wanted_services,
                    exclude_node_ids,
                }
            }
            MSG_DISCOVERY_PEERS => Self::Peers {
                records: decode_record_list(&mut reader, MAX_DISCOVERY_RECORDS_PER_RESPONSE)?,
            },
            MSG_DISCOVERY_GET_SERVICES => Self::GetServices(decode_get_services(&mut reader)?),
            MSG_DISCOVERY_SERVICES => Self::Services(decode_services_message(&mut reader)?),
            value => return Err(DiscoveryWireError::InvalidMessageType(value)),
        };
        reject_trailing(bytes, &reader)?;
        Ok(message)
    }
}

/// A malformed discovery wire message.
#[derive(Error, Debug)]
pub enum DiscoveryWireError {
    /// A payload exceeded its hard cap.
    #[error("Zakura discovery payload length {actual} exceeds hard cap {max}")]
    OversizedPayload {
        /// Actual payload length.
        actual: usize,
        /// Maximum allowed payload length.
        max: usize,
    },

    /// An I/O error while encoding or decoding.
    #[error("Zakura discovery wire I/O error: {0}")]
    Io(#[from] io::Error),

    /// A length-delimited field claimed more bytes than remain in the bounded buffer.
    #[error("Zakura discovery field declared {declared} bytes with only {remaining} remaining")]
    TruncatedPayload {
        /// Declared field length.
        declared: usize,
        /// Remaining bytes in the bounded parent buffer.
        remaining: usize,
    },

    /// The discovery message type is unknown.
    #[error("invalid Zakura discovery message type {0}")]
    InvalidMessageType(u8),

    /// The network id is unknown.
    #[error("invalid Zakura discovery network id {0}")]
    InvalidNetworkId(u32),

    /// A record address family is unknown.
    #[error("invalid Zakura discovery socket address family {0}")]
    InvalidAddressFamily(u8),

    /// A node id did not decode into a valid iroh public key.
    #[error("invalid Zakura discovery node id")]
    InvalidNodeId,

    /// A first-party live field described a different node than the authenticated peer.
    #[error("Zakura discovery {field} does not match authenticated peer node id")]
    MismatchedNodeId {
        /// Field name.
        field: &'static str,
    },

    /// A decoded payload had trailing bytes.
    #[error("trailing bytes in Zakura discovery payload")]
    TrailingBytes,

    /// A required field was empty.
    #[error("empty Zakura discovery {0}")]
    Empty(&'static str),

    /// A service id was not ASCII.
    #[error("Zakura discovery service id is not ASCII")]
    NonAsciiServiceId,

    /// A boolean field was not encoded as 0 or 1.
    #[error("invalid Zakura discovery boolean {field} value {value}")]
    InvalidBoolean {
        /// Field name.
        field: &'static str,
        /// Encoded value.
        value: u8,
    },

    /// A known summary tag was attached to the wrong service id.
    #[error("Zakura discovery summary tag {summary_tag} does not match service id {service_id}")]
    MismatchedSummaryService {
        /// Versioned summary payload tag.
        summary_tag: u16,
        /// Service id from the enclosing summary envelope.
        service_id: ZakuraServiceId,
    },

    /// A block height exceeded Zebra's accepted height range.
    #[error("invalid Zakura discovery block height {0}")]
    InvalidHeight(u32),

    /// A numeric conversion failed while handling bounded data.
    #[error("numeric overflow while encoding Zakura discovery {0}")]
    NumericOverflow(&'static str),

    /// The signing key does not match the record author.
    #[error("Zakura discovery signing key does not match record node id")]
    SigningKeyMismatch,

    /// The record has an invalid protocol range.
    #[error("invalid Zakura discovery protocol range")]
    InvalidProtocolRange,
}

/// A signed record failed validation for import.
#[derive(Error, Debug)]
pub enum DiscoveryRecordError {
    /// The record wire shape or local bounds are invalid.
    #[error(transparent)]
    Wire(#[from] DiscoveryWireError),

    /// The record's public key bytes are malformed.
    #[error("malformed Zakura discovery node id")]
    MalformedNodeId,

    /// The record signature does not verify.
    #[error("invalid Zakura discovery record signature")]
    InvalidSignature,

    /// The record is expired beyond clock skew tolerance.
    #[error("expired Zakura discovery record")]
    Expired,

    /// The record expires too far in the future.
    #[error("far-future Zakura discovery record expiry")]
    FarFutureExpiry,

    /// The record is for another network.
    #[error("wrong Zakura discovery network")]
    WrongNetwork,

    /// The record is for another chain.
    #[error("wrong Zakura discovery chain")]
    WrongChain,

    /// The record's protocol range is invalid or incompatible.
    #[error("incompatible Zakura discovery protocol range")]
    IncompatibleProtocol,
}

/// Default maximum discovery book entries learned from peers.
pub const DEFAULT_MAX_DISCOVERY_BOOK_RECORDS: usize = 10_000;

const HIGH_DIAL_FAILURE_COUNT: u32 = 3;

/// Local storage limits for the native Zakura discovery book.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct ZakuraDiscoveryBookLimits {
    /// Maximum non-static records learned from peers.
    pub max_records: usize,
    /// Maximum records imported from one peer response.
    pub max_imported_records_per_response: usize,
    /// Maximum direct addresses in an imported record.
    pub max_direct_addrs_per_record: usize,
    /// Maximum services in an imported record.
    pub max_services_per_record: usize,
    /// Maximum encoded record size accepted by the book.
    pub max_encoded_record_bytes: usize,
}

impl Default for ZakuraDiscoveryBookLimits {
    fn default() -> Self {
        Self {
            max_records: DEFAULT_MAX_DISCOVERY_BOOK_RECORDS,
            max_imported_records_per_response: MAX_DISCOVERY_RECORDS_PER_RESPONSE,
            max_direct_addrs_per_record: MAX_DIRECT_ADDRS_PER_RECORD,
            max_services_per_record: MAX_SERVICES_PER_RECORD,
            max_encoded_record_bytes: MAX_DISCOVERY_MESSAGE_BYTES,
        }
    }
}

/// A stored Zakura discovery record with local dial metadata.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ZakuraDiscoveryEntry {
    record: ZakuraNodeRecord,
    source: Option<NodeId>,
    is_static: bool,
    last_seen: u64,
    last_dial_attempt: Option<u64>,
    last_success: Option<u64>,
    last_short_lived_exchange: Option<u64>,
    failure_count: u32,
}

impl ZakuraDiscoveryEntry {
    /// Returns the latest valid signed record for this node.
    pub fn record(&self) -> &ZakuraNodeRecord {
        &self.record
    }

    /// Returns the peer that supplied this record, if any.
    pub fn source(&self) -> Option<NodeId> {
        self.source
    }

    /// Returns true when this entry came from static/bootstrap configuration.
    pub fn is_static(&self) -> bool {
        self.is_static
    }

    /// Returns when this entry was last seen as a Unix timestamp.
    pub fn last_seen(&self) -> u64 {
        self.last_seen
    }

    /// Returns the last dial attempt as a Unix timestamp.
    pub fn last_dial_attempt(&self) -> Option<u64> {
        self.last_dial_attempt
    }

    /// Returns the last successful dial as a Unix timestamp.
    pub fn last_success(&self) -> Option<u64> {
        self.last_success
    }

    /// Returns the last successful short-lived discovery exchange as a Unix timestamp.
    pub fn last_short_lived_exchange(&self) -> Option<u64> {
        self.last_short_lived_exchange
    }

    /// Returns the consecutive dial failure count.
    pub fn failure_count(&self) -> u32 {
        self.failure_count
    }
}

/// A persistence-layer entry for the discovery book.
///
/// This type intentionally contains no filesystem behavior. Cache readers and
/// writers can serialize these values, then re-import them through the book so
/// every loaded record is re-validated before use.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ZakuraDiscoveryPersistedEntry {
    /// The signed node record.
    pub record: ZakuraNodeRecord,
    /// The peer that supplied the record, if any.
    pub source: Option<NodeId>,
    /// Whether this entry came from static/bootstrap configuration.
    pub is_static: bool,
    /// Last seen Unix timestamp.
    pub last_seen: u64,
    /// Last dial attempt Unix timestamp.
    pub last_dial_attempt: Option<u64>,
    /// Last successful dial Unix timestamp.
    pub last_success: Option<u64>,
    /// Consecutive dial failure count.
    pub failure_count: u32,
}

/// A locally dialable discovery candidate.
///
/// Candidates may come from first-party confirmed signed discovery records or from trusted static
/// bootstrap configuration. Only signed records are eligible for peer samples; unsigned static
/// candidates are local dial hints.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ZakuraDiscoveryDialCandidate {
    /// Candidate iroh node id.
    pub node_id: NodeId,
    /// Direct addresses to pass to iroh for dialing.
    pub direct_addrs: Vec<SocketAddr>,
    /// Whether this candidate came from static/operator configuration.
    pub is_static: bool,
}

impl ZakuraDiscoveryDialCandidate {
    /// Converts this candidate into an iroh dial address.
    pub fn node_addr(&self) -> NodeAddr {
        NodeAddr::new(self.node_id).with_direct_addresses(self.direct_addrs.clone())
    }
}

/// Service-aware peer candidates derived from signed discovery state.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ZakuraServiceCandidates {
    /// Connected peers whose latest valid `Hello` advertised the requested service.
    pub connected: Vec<NodeId>,
    /// Dialable discovered peers whose signed record advertised the requested service.
    pub discovered: Vec<ZakuraDiscoveryDialCandidate>,
    /// Whether discovered candidates came from explicit fallback to general peers.
    pub used_fallback: bool,
}

/// Header-sync candidate selection hints owned by the header-sync reactor.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ZakuraHeaderSyncCandidateState {
    /// Lowest header height that would make a new peer useful.
    pub target_height: block::Height,
    /// Peers already admitted by header sync; advisory "full" summaries do not remove them.
    pub admitted_node_ids: Vec<NodeId>,
    /// Peers in local, non-punitive advisory backoff after failing to confirm usefulness.
    pub backed_off_node_ids: Vec<NodeId>,
}

impl Default for ZakuraHeaderSyncCandidateState {
    fn default() -> Self {
        Self {
            target_height: block::Height::MIN,
            admitted_node_ids: Vec::new(),
            backed_off_node_ids: Vec::new(),
        }
    }
}

/// Block-sync candidate selection hints owned by the block-sync reactor.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ZakuraBlockSyncCandidateState {
    /// Header-known body heights currently missing from local state.
    pub missing_block_bodies: Vec<block::Height>,
    /// Peers already admitted by block sync; advisory "full" summaries do not remove them.
    pub admitted_node_ids: Vec<NodeId>,
}

/// The result of importing one signed discovery record.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum ImportOutcome {
    /// A new node entry was inserted.
    Added,
    /// A newer signed record replaced the previous signed record.
    Updated,
    /// An equal-sequence record refreshed local metadata only.
    MetadataUpdated,
    /// An older record was valid but ignored.
    IgnoredOlder,
}

/// Summary of importing a bounded batch of discovery records.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct ImportBatchOutcome {
    /// Number of records considered before the per-response cap stopped import.
    pub attempted: usize,
    /// Number of newly added records.
    pub added: usize,
    /// Number of updated records.
    pub updated: usize,
    /// Number of metadata-only updates.
    pub metadata_updated: usize,
    /// Number of older records ignored.
    pub ignored_older: usize,
    /// Number of records rejected by validation.
    pub rejected: usize,
    /// Number of records skipped because the response exceeded the import cap.
    pub dropped_for_limit: usize,
}

impl ImportBatchOutcome {
    fn record_success(&mut self, outcome: ImportOutcome) {
        match outcome {
            ImportOutcome::Added => self.added += 1,
            ImportOutcome::Updated => self.updated += 1,
            ImportOutcome::MetadataUpdated => self.metadata_updated += 1,
            ImportOutcome::IgnoredOlder => self.ignored_older += 1,
        }
    }
}

/// A discovery book import or storage error.
#[derive(Error, Debug)]
pub enum DiscoveryBookError {
    /// The signed record failed validation.
    #[error(transparent)]
    Record(#[from] DiscoveryRecordError),

    /// The record describes the local node and self-record import was not requested.
    #[error("Zakura discovery book rejected local self-record")]
    SelfRecord,

    /// A connected peer's self-record did not match its authenticated identity.
    #[error("Zakura discovery connected self-record author does not match authenticated peer")]
    MismatchedConnectedPeerRecord,

    /// The record has no direct address usable for a future dial attempt.
    #[error("Zakura discovery record has no usable direct address")]
    NoUsableDirectAddress,

    /// The record contains a direct address that must not be used for discovery.
    #[error("Zakura discovery record contains non-dialable direct address {addr}")]
    NonDialableDirectAddress {
        /// The rejected direct address.
        addr: SocketAddr,
    },

    /// A record field exceeded a book-specific storage limit.
    #[error("Zakura discovery record {field} count {actual} exceeds book limit {max}")]
    Limit {
        /// The limited field.
        field: &'static str,
        /// Actual count or size.
        actual: usize,
        /// Maximum accepted count or size.
        max: usize,
    },
}

/// In-memory storage for signed Zakura discovery records.
#[derive(Clone, Debug)]
pub struct ZakuraDiscoveryBook {
    entries: HashMap<NodeId, ZakuraDiscoveryEntry>,
    static_candidates: HashMap<NodeId, ZakuraStaticDiscoveryCandidate>,
    limits: ZakuraDiscoveryBookLimits,
    local_node_id: Option<NodeId>,
}

/// Passive runtime limits and timing policy for native Zakura discovery.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ZakuraDiscoveryConfig {
    /// Lifetime of locally authored self-records.
    pub record_ttl: Duration,
    /// Minimum interval for future active refresh logic.
    pub refresh_interval: Duration,
    /// Default peer sample size for future exchange logic.
    pub peer_sample_limit: usize,
    /// Discovery book storage limits.
    pub book_limits: ZakuraDiscoveryBookLimits,
    /// Maximum node ids accepted in an exclusion list.
    pub max_excluded_node_ids: usize,
    /// Base backoff for future discovery dials.
    pub dial_backoff_base: Duration,
    /// Maximum backoff for future discovery dials.
    pub dial_backoff_max: Duration,
    /// Maximum concurrent future discovery dials.
    pub max_concurrent_discovery_dials: usize,
    /// Slots reserved below the Zakura connection cap for maintained peers.
    pub discovery_connection_headroom: usize,
    /// Total Zakura connection cap used to derive the discovery soft cap.
    pub max_zakura_connections: usize,
    /// Discovery peer caps and queue limits owned by this runtime.
    pub peer_limits: ServicePeerLimits,
    /// Maximum accepted future TTL on imported records.
    pub max_record_ttl: Duration,
    /// Accepted clock skew around expiry checks.
    pub clock_skew_tolerance: Duration,
}

impl Default for ZakuraDiscoveryConfig {
    fn default() -> Self {
        Self {
            record_ttl: DEFAULT_DISCOVERY_RECORD_TTL,
            refresh_interval: DEFAULT_DISCOVERY_REFRESH_INTERVAL,
            peer_sample_limit: DEFAULT_DISCOVERY_PEER_SAMPLE_LIMIT,
            book_limits: ZakuraDiscoveryBookLimits::default(),
            max_excluded_node_ids: MAX_DISCOVERY_EXCLUDED_NODE_IDS,
            dial_backoff_base: DEFAULT_DISCOVERY_DIAL_BACKOFF_BASE,
            dial_backoff_max: DEFAULT_DISCOVERY_DIAL_BACKOFF_MAX,
            max_concurrent_discovery_dials: DEFAULT_MAX_CONCURRENT_DISCOVERY_DIALS,
            discovery_connection_headroom: DEFAULT_DISCOVERY_CONNECTION_HEADROOM,
            max_zakura_connections: DEFAULT_DISCOVERY_ZAKURA_MAX_CONNECTIONS,
            peer_limits: ServicePeerLimits::default(),
            max_record_ttl: DEFAULT_DISCOVERY_MAX_RECORD_TTL,
            clock_skew_tolerance: DEFAULT_DISCOVERY_CLOCK_SKEW_TOLERANCE,
        }
    }
}

/// Construction inputs for the locally authored Zakura discovery self-record.
#[derive(Clone)]
pub struct ZakuraDiscoveryLocalConfig {
    /// Local iroh secret key used to sign self-records.
    pub secret_key: SecretKey,
    /// Configured direct listener addresses to advertise if routable.
    pub direct_addrs: Vec<SocketAddr>,
    /// Locally supported native Zakura services.
    pub services: Vec<ZakuraServiceId>,
    /// Lowest supported Zakura protocol version.
    pub zakura_protocol_min: u16,
    /// Highest supported Zakura protocol version.
    pub zakura_protocol_max: u16,
    /// Local Zakura network id.
    pub network_id: ZakuraNetworkId,
    /// Local genesis hash / chain id.
    pub chain_id: [u8; 32],
    /// Last locally authored self-record sequence loaded from durable storage.
    ///
    /// Until cache persistence wires this seed at startup, self-record sequences are derived from
    /// wall-clock nanoseconds. They strictly increase across a same-key restart only while the
    /// system clock is monotonic; a backward clock step can still produce a non-increasing sequence.
    pub last_authored_sequence: Option<u64>,
}

impl fmt::Debug for ZakuraDiscoveryLocalConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ZakuraDiscoveryLocalConfig")
            .field("node_id", &self.secret_key.public())
            .field("direct_addrs", &self.direct_addrs)
            .field("services", &self.services)
            .field("zakura_protocol_min", &self.zakura_protocol_min)
            .field("zakura_protocol_max", &self.zakura_protocol_max)
            .field("network_id", &self.network_id)
            .field("chain_id", &self.chain_id)
            .field("last_authored_sequence", &self.last_authored_sequence)
            .finish()
    }
}

/// Cloneable passive runtime handle for native Zakura discovery state.
#[derive(Clone)]
pub struct ZakuraDiscoveryHandle {
    inner: Arc<Mutex<ZakuraDiscoveryInner>>,
    connected: watch::Receiver<Vec<ZakuraPeerId>>,
    self_record: watch::Receiver<Arc<ZakuraNodeRecord>>,
    self_record_tx: watch::Sender<Arc<ZakuraNodeRecord>>,
    peer_snapshot: watch::Receiver<ServicePeerSnapshot>,
    peer_snapshot_tx: watch::Sender<ServicePeerSnapshot>,
    import_validation: DiscoveryImportValidation,
}

/// Immutable inputs needed to validate and bound a peer record batch without holding the
/// global discovery mutex.
///
/// These mirror the fields [`ZakuraDiscoveryInner::validation_context`] reads, all of which
/// are fixed at construction (local identity/network parameters and import limits never
/// change at runtime). Snapshotting them on the handle lets [`ZakuraDiscoveryHandle::import_peer_records`]
/// run CPU-heavy Ed25519 verification outside the lock.
#[derive(Clone, Debug)]
struct DiscoveryImportValidation {
    expected_network_id: ZakuraNetworkId,
    expected_chain_id: [u8; 32],
    supported_protocol_min: u16,
    supported_protocol_max: u16,
    max_record_ttl: Duration,
    clock_skew_tolerance: Duration,
    max_imported_records_per_response: usize,
}

impl DiscoveryImportValidation {
    fn context(&self, now: u64) -> DiscoveryRecordValidationContext {
        DiscoveryRecordValidationContext {
            expected_network_id: self.expected_network_id,
            expected_chain_id: self.expected_chain_id,
            current_unix_secs: now,
            supported_protocol_min: self.supported_protocol_min,
            supported_protocol_max: self.supported_protocol_max,
            max_record_ttl: self.max_record_ttl,
            clock_skew_tolerance: self.clock_skew_tolerance,
        }
    }
}

impl fmt::Debug for ZakuraDiscoveryHandle {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ZakuraDiscoveryHandle")
            .field(
                "self_record_node_id",
                &self.self_record.borrow().body.node_id,
            )
            .finish_non_exhaustive()
    }
}

#[derive(Debug)]
struct ZakuraDiscoveryInner {
    book: ZakuraDiscoveryBook,
    active_services: HashMap<NodeId, ZakuraActiveServiceEntry>,
    admitted_peers: HashMap<ZakuraPeerId, ServicePeerDirection>,
    last_connected_node_ids: HashSet<NodeId>,
    local: ZakuraLocalDiscoveryState,
    config: ZakuraDiscoveryConfig,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ZakuraActiveServiceEntry {
    record: Option<ZakuraNodeRecord>,
    services: Vec<ZakuraServiceId>,
    live_summaries: Vec<ZakuraCachedLiveServiceSummary>,
}

impl ZakuraActiveServiceEntry {
    fn from_record(record: ZakuraNodeRecord, services: Vec<ZakuraServiceId>) -> Self {
        Self {
            record: Some(record),
            services,
            live_summaries: Vec::new(),
        }
    }

    fn from_live_summaries(summaries: Vec<ZakuraCachedLiveServiceSummary>) -> Self {
        let mut services = Vec::new();
        for summary in &summaries {
            push_unique_service(&mut services, summary.service_id.clone());
        }
        Self {
            record: None,
            services,
            live_summaries: summaries,
        }
    }

    fn update_record(&mut self, record: ZakuraNodeRecord, services: Vec<ZakuraServiceId>) {
        self.record = Some(record);
        self.services = services;
    }

    fn update_live_summaries(&mut self, summaries: Vec<ZakuraCachedLiveServiceSummary>) {
        self.live_summaries.retain(|cached| {
            !summaries.iter().any(|summary| {
                summary.service_id == cached.service_id && summary.summary_tag == cached.summary_tag
            })
        });
        for summary in summaries {
            self.live_summaries.push(summary);
        }
        if self.record.is_none() {
            self.refresh_live_derived_services();
        }
    }

    fn prune_expired_live_summaries(&mut self, now_unix_secs: u64) {
        self.live_summaries
            .retain(|summary| summary.expires_at_unix_secs > now_unix_secs);
        if self.record.is_none() {
            self.refresh_live_derived_services();
        }
    }

    fn refresh_live_derived_services(&mut self) {
        let mut services = Vec::new();
        for summary in &self.live_summaries {
            push_unique_service(&mut services, summary.service_id.clone());
        }
        self.services = services;
    }

    fn is_empty_live_only(&self) -> bool {
        self.record.is_none() && self.live_summaries.is_empty()
    }

    fn live_summary_preference(&self, service: &ZakuraServiceId, now_unix_secs: u64) -> u32 {
        self.live_summaries
            .iter()
            .filter(|summary| {
                summary.service_id == *service && summary.expires_at_unix_secs > now_unix_secs
            })
            .map(ZakuraCachedLiveServiceSummary::preference_score)
            .max()
            .unwrap_or(0)
    }

    fn fresh_header_sync_summary(&self, now_unix_secs: u64) -> Option<HeaderSyncServiceSummary> {
        self.live_summaries.iter().find_map(|summary| {
            if summary.service_id != ZakuraServiceId::header_sync()
                || summary.expires_at_unix_secs <= now_unix_secs
            {
                return None;
            }
            match summary.summary {
                ZakuraLiveServiceSummary::HeaderSync(header_sync) => Some(header_sync),
                ZakuraLiveServiceSummary::BlockSync(_)
                | ZakuraLiveServiceSummary::Discovery(_)
                | ZakuraLiveServiceSummary::Unknown => None,
            }
        })
    }

    fn fresh_block_sync_summary(&self, now_unix_secs: u64) -> Option<BlockSyncServiceSummary> {
        self.live_summaries.iter().find_map(|summary| {
            if summary.service_id != ZakuraServiceId::block_sync()
                || summary.expires_at_unix_secs <= now_unix_secs
            {
                return None;
            }
            match summary.summary {
                ZakuraLiveServiceSummary::BlockSync(block_sync) => Some(block_sync),
                ZakuraLiveServiceSummary::HeaderSync(_)
                | ZakuraLiveServiceSummary::Discovery(_)
                | ZakuraLiveServiceSummary::Unknown => None,
            }
        })
    }
}

/// A decoded first-party live service summary cached while its peer remains connected.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ZakuraCachedLiveServiceSummary {
    /// Service this cached summary describes.
    pub service_id: ZakuraServiceId,
    /// Versioned summary payload tag.
    pub summary_tag: u16,
    /// Decoded payload for known service summary tags.
    pub summary: ZakuraLiveServiceSummary,
    /// Local Unix timestamp when this summary was observed.
    pub observed_at_unix_secs: u64,
    /// Effective local expiry, clamped to the local live-summary TTL.
    pub expires_at_unix_secs: u64,
}

impl ZakuraCachedLiveServiceSummary {
    fn from_envelope(
        envelope: &ServiceSummaryEnvelope,
        observed_at_unix_secs: u64,
        expires_at_unix_secs: u64,
    ) -> Result<Self, DiscoveryWireError> {
        let summary = if let Some(summary) = envelope.decode_header_sync()? {
            ZakuraLiveServiceSummary::HeaderSync(summary)
        } else if let Some(summary) = envelope.decode_discovery()? {
            ZakuraLiveServiceSummary::Discovery(summary)
        } else if let Some(summary) = envelope.decode_block_sync()? {
            ZakuraLiveServiceSummary::BlockSync(summary)
        } else {
            ZakuraLiveServiceSummary::Unknown
        };
        Ok(Self {
            service_id: envelope.service_id.clone(),
            summary_tag: envelope.summary_tag,
            summary,
            observed_at_unix_secs,
            expires_at_unix_secs,
        })
    }

    fn preference_score(&self) -> u32 {
        match self.summary {
            ZakuraLiveServiceSummary::HeaderSync(summary) => {
                if !summary.serving_headers {
                    return 0;
                }
                u32::from(summary.inbound_slots_free)
                    .saturating_add(u32::from(summary.outbound_slots_free))
            }
            ZakuraLiveServiceSummary::Discovery(summary) => {
                u32::from(summary.peer_exchange_slots_free)
            }
            ZakuraLiveServiceSummary::BlockSync(summary) => u32::from(summary.free_slots)
                .saturating_add(
                    summary
                        .max_blocks_per_response
                        .min(MAX_BS_BLOCKS_PER_REQUEST),
                ),
            ZakuraLiveServiceSummary::Unknown => 1,
        }
    }
}

/// Decoded payload for a cached first-party live service summary.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum ZakuraLiveServiceSummary {
    /// Header-sync dial/admission hint.
    HeaderSync(HeaderSyncServiceSummary),
    /// Discovery exchange hint.
    Discovery(DiscoveryServiceSummary),
    /// Block-sync dial/admission hint.
    BlockSync(BlockSyncServiceSummary),
    /// Unknown or reserved summary tag, kept only as a freshness hint.
    Unknown,
}

#[derive(Clone)]
struct ZakuraLocalDiscoveryState {
    secret_key: SecretKey,
    node_id: NodeId,
    direct_addrs: Vec<SocketAddr>,
    services: Vec<ZakuraServiceId>,
    zakura_protocol_min: u16,
    zakura_protocol_max: u16,
    network_id: ZakuraNetworkId,
    chain_id: [u8; 32],
    next_sequence: u64,
}

impl fmt::Debug for ZakuraLocalDiscoveryState {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ZakuraLocalDiscoveryState")
            .field("node_id", &self.node_id)
            .field("direct_addrs", &self.direct_addrs)
            .field("services", &self.services)
            .field("zakura_protocol_min", &self.zakura_protocol_min)
            .field("zakura_protocol_max", &self.zakura_protocol_max)
            .field("network_id", &self.network_id)
            .field("chain_id", &self.chain_id)
            .field("next_sequence", &self.next_sequence)
            .finish()
    }
}

impl ZakuraDiscoveryHandle {
    /// Creates passive discovery state using the current wall-clock sequence.
    pub fn new(
        local_config: ZakuraDiscoveryLocalConfig,
        config: ZakuraDiscoveryConfig,
        connected: watch::Receiver<Vec<ZakuraPeerId>>,
    ) -> Result<Self, DiscoveryWireError> {
        Self::new_at(
            local_config,
            config,
            connected,
            current_unix_secs(),
            current_sequence_tick(),
        )
    }

    fn new_at(
        local_config: ZakuraDiscoveryLocalConfig,
        config: ZakuraDiscoveryConfig,
        connected: watch::Receiver<Vec<ZakuraPeerId>>,
        now_unix_secs: u64,
        wall_clock_sequence: u64,
    ) -> Result<Self, DiscoveryWireError> {
        let mut local = ZakuraLocalDiscoveryState::new(local_config, wall_clock_sequence);
        let self_record = Arc::new(local.build_self_record(
            now_unix_secs,
            wall_clock_sequence,
            config.record_ttl,
        )?);
        let (self_record_tx, self_record_rx) = watch::channel(self_record);
        let (peer_snapshot_tx, peer_snapshot_rx) =
            watch::channel(ServicePeerSnapshot::new(0, 0, config.peer_limits));
        let book = ZakuraDiscoveryBook::with_local_node_id(config.book_limits, local.node_id);
        let import_validation = DiscoveryImportValidation {
            expected_network_id: local.network_id,
            expected_chain_id: local.chain_id,
            supported_protocol_min: local.zakura_protocol_min,
            supported_protocol_max: local.zakura_protocol_max,
            max_record_ttl: config.max_record_ttl,
            clock_skew_tolerance: config.clock_skew_tolerance,
            max_imported_records_per_response: config.book_limits.max_imported_records_per_response,
        };
        Ok(Self {
            inner: Arc::new(Mutex::new(ZakuraDiscoveryInner {
                book,
                active_services: HashMap::new(),
                admitted_peers: HashMap::new(),
                last_connected_node_ids: HashSet::new(),
                local,
                config,
            })),
            connected,
            self_record: self_record_rx,
            self_record_tx,
            peer_snapshot: peer_snapshot_rx,
            peer_snapshot_tx,
            import_validation,
        })
    }

    /// Returns the current signed local self-record without taking the state lock.
    pub fn current_self_record(&self) -> Arc<ZakuraNodeRecord> {
        self.self_record.borrow().clone()
    }

    /// Returns the local node id used in authored self-records.
    pub fn local_node_id(&self) -> NodeId {
        self.self_record.borrow().body.node_id
    }

    /// Returns the configured peer sample limit for active exchanges.
    pub async fn peer_sample_limit(&self) -> usize {
        let inner = self.inner.lock().await;
        inner.config.peer_sample_limit
    }

    /// Returns the configured interval between discovery refresh exchanges.
    pub async fn refresh_interval(&self) -> Duration {
        let inner = self.inner.lock().await;
        inner.config.refresh_interval
    }

    /// Return the currently cached discovery peer slot snapshot.
    pub fn peer_snapshot(&self) -> ServicePeerSnapshot {
        *self.peer_snapshot.borrow()
    }

    /// Subscribe to discovery peer slot snapshots.
    pub fn subscribe_peer_snapshot(&self) -> watch::Receiver<ServicePeerSnapshot> {
        self.peer_snapshot.clone()
    }

    /// Build this node's first-party discovery service summary.
    pub async fn local_discovery_summary(&self) -> DiscoveryServiceSummary {
        DiscoveryServiceSummary::from_snapshot(
            self.peer_snapshot(),
            self.peer_sample_limit().await,
            true,
        )
    }

    /// Wrap first-party live summaries in a short-lived service response.
    pub fn local_services_response(&self, summaries: Vec<ServiceSummaryEnvelope>) -> Services {
        Services {
            node_id: self.local_node_id(),
            expires_at_unix_secs: current_unix_secs()
                .saturating_add(DEFAULT_LIVE_SERVICE_SUMMARY_TTL.as_secs()),
            summaries,
        }
    }

    /// Admit a typed discovery session if the service-specific cap has room.
    pub async fn admit_peer(
        &self,
        peer_id: ZakuraPeerId,
        direction: ServicePeerDirection,
    ) -> ServiceAdmissionDecision {
        let mut inner = self.inner.lock().await;
        if let Some(peer_direction) = inner.admitted_peers.get_mut(&peer_id) {
            *peer_direction = direction;
            self.publish_peer_snapshot_locked(&inner);
            return ServiceAdmissionDecision::Admit;
        }

        let admitted = inner
            .admitted_peers
            .values()
            .filter(|peer_direction| **peer_direction == direction)
            .count();
        let cap = match direction {
            ServicePeerDirection::Inbound => inner.config.peer_limits.max_inbound_peers,
            ServicePeerDirection::Outbound => inner.config.peer_limits.max_outbound_peers,
        };

        if admitted >= cap {
            ServiceAdmissionDecision::RejectFull
        } else {
            inner.admitted_peers.insert(peer_id, direction);
            self.publish_peer_snapshot_locked(&inner);
            ServiceAdmissionDecision::Admit
        }
    }

    /// Remove a locally closed or disconnected discovery service session.
    pub async fn remove_peer(&self, peer_id: &ZakuraPeerId) {
        let mut inner = self.inner.lock().await;
        inner.admitted_peers.remove(peer_id);
        self.publish_peer_snapshot_locked(&inner);
    }

    fn publish_peer_snapshot_locked(&self, inner: &ZakuraDiscoveryInner) {
        let inbound = inner
            .admitted_peers
            .values()
            .filter(|direction| **direction == ServicePeerDirection::Inbound)
            .count();
        let outbound = inner
            .admitted_peers
            .values()
            .filter(|direction| **direction == ServicePeerDirection::Outbound)
            .count();
        let _ = self.peer_snapshot_tx.send(ServicePeerSnapshot::new(
            inbound,
            outbound,
            inner.config.peer_limits,
        ));
    }

    /// Returns bounded node ids to exclude from a peer-sample request.
    ///
    /// Exclusions include the local node, currently connected peers, and recent known records.
    pub async fn peer_sample_exclusions(&self) -> Vec<NodeId> {
        let connected = connected_peer_node_ids(&self.connected.borrow());
        let inner = self.inner.lock().await;
        let mut excluded = Vec::with_capacity(inner.config.max_excluded_node_ids);
        push_excluded_node_id(
            &mut excluded,
            inner.local.node_id,
            inner.config.max_excluded_node_ids,
        );
        for node_id in connected {
            push_excluded_node_id(&mut excluded, node_id, inner.config.max_excluded_node_ids);
        }
        for node_id in inner.book.recent_node_ids(
            inner
                .config
                .max_excluded_node_ids
                .saturating_sub(excluded.len()),
        ) {
            push_excluded_node_id(&mut excluded, node_id, inner.config.max_excluded_node_ids);
        }
        excluded
    }

    /// Replaces the local advertised service set and signs a newer self-record.
    pub async fn update_advertised_services(
        &self,
        services: Vec<ZakuraServiceId>,
    ) -> Result<Arc<ZakuraNodeRecord>, DiscoveryWireError> {
        let services = normalize_services(services);
        if self.self_record.borrow().body.services == services {
            return Ok(self.current_self_record());
        }

        let record = {
            let mut inner = self.inner.lock().await;
            inner.local.services = services;
            let record_ttl = inner.config.record_ttl;
            Arc::new(inner.local.build_self_record(
                current_unix_secs(),
                current_sequence_tick(),
                record_ttl,
            )?)
        };
        self.self_record_tx.send_replace(record.clone());
        Ok(record)
    }

    /// Imports peer-supplied signed records through the discovery book validation path.
    ///
    /// Record signatures are verified OUTSIDE the global discovery mutex. Ed25519
    /// verification and signature-domain re-encoding are CPU-heavy and fully
    /// attacker-driven (an authenticated discovery peer can send a full `Peers` batch of
    /// records with valid or invalid signatures, repeatedly); running them while holding
    /// the shared async lock lets that peer stall unrelated discovery admission,
    /// sampling, and dial selection. Only the cheap book mutation (storage-limit recheck,
    /// address policy, map insert, eviction) runs under the lock, and the lock is skipped
    /// entirely when nothing survives verification. See finding
    /// `claude-discovery-expensive-work-under-global-mutex` (SR-2).
    pub async fn import_peer_records(
        &self,
        records: impl IntoIterator<Item = ZakuraNodeRecord>,
        source: Option<NodeId>,
    ) -> ImportBatchOutcome {
        let now = current_unix_secs();
        let context = self.import_validation.context(now);
        let max_records = self.import_validation.max_imported_records_per_response;

        let mut outcome = ImportBatchOutcome::default();
        let mut verified = Vec::new();
        for record in records {
            if outcome.attempted >= max_records {
                outcome.dropped_for_limit += 1;
                continue;
            }
            outcome.attempted += 1;
            match record.verify(&context) {
                Ok(()) => verified.push(record),
                Err(_) => outcome.rejected += 1,
            }
        }

        if !verified.is_empty() {
            let mut inner = self.inner.lock().await;
            inner
                .book
                .import_pre_verified_records(verified, source, now, &context, &mut outcome);
        }

        outcome
    }

    /// Imports one peer-supplied signed record through the discovery book validation path.
    pub async fn import_peer_record(
        &self,
        record: ZakuraNodeRecord,
        source: Option<NodeId>,
    ) -> Result<ImportOutcome, DiscoveryBookError> {
        let now = current_unix_secs();
        let mut inner = self.inner.lock().await;
        let context = inner.validation_context(now);
        inner.book.import_record(record, source, now, &context)
    }

    /// Imports a connected peer's first-party live service summaries.
    ///
    /// Live summaries are accepted only from the authenticated peer they describe, are bounded by
    /// the connected-peer set, and expire independently of durable signed node records.
    pub async fn import_connected_peer_services(
        &self,
        services: Services,
        peer_node_id: NodeId,
    ) -> Result<(), DiscoveryWireError> {
        self.import_connected_peer_services_at(services, peer_node_id, current_unix_secs())
            .await
    }

    async fn import_connected_peer_services_at(
        &self,
        services: Services,
        peer_node_id: NodeId,
        now: u64,
    ) -> Result<(), DiscoveryWireError> {
        if services.node_id != peer_node_id {
            return Err(DiscoveryWireError::MismatchedNodeId {
                field: "services node id",
            });
        }

        let connected_node_ids = connected_peer_node_ids(&self.connected.borrow());
        let mut inner = self.inner.lock().await;
        inner.sync_active_services(&connected_node_ids, now);
        if !connected_node_ids.contains(&peer_node_id) {
            return Ok(());
        }

        let Some(expires_at_unix_secs) =
            effective_live_summary_expiry(services.expires_at_unix_secs, now)
        else {
            if let Some(entry) = inner.active_services.get_mut(&peer_node_id) {
                entry.live_summaries.clear();
                if entry.record.is_none() {
                    entry.refresh_live_derived_services();
                }
            }
            inner
                .active_services
                .retain(|_, entry| !entry.is_empty_live_only());
            return Ok(());
        };

        let mut live_summaries = Vec::with_capacity(services.summaries.len());
        for envelope in &services.summaries {
            live_summaries.push(ZakuraCachedLiveServiceSummary::from_envelope(
                envelope,
                now,
                expires_at_unix_secs,
            )?);
        }

        inner
            .active_services
            .entry(peer_node_id)
            .and_modify(|entry| entry.update_live_summaries(live_summaries.clone()))
            .or_insert_with(|| ZakuraActiveServiceEntry::from_live_summaries(live_summaries));
        Ok(())
    }

    /// Imports one trusted static signed record through the static validation path.
    pub async fn import_static_record(
        &self,
        record: ZakuraNodeRecord,
    ) -> Result<ImportOutcome, DiscoveryBookError> {
        let now = current_unix_secs();
        let mut inner = self.inner.lock().await;
        let context = inner.validation_context(now);
        inner.book.import_static_record(record, now, &context)
    }

    /// Imports a connected peer's latest self-record and updates active service hints.
    ///
    /// Active service hints come only from the authenticated peer's own signed `Hello`.
    /// Addressless or locally non-dialable records can still update active services, because the
    /// live connection is already established; they are still rejected from dialable storage.
    pub async fn import_connected_peer_record(
        &self,
        record: ZakuraNodeRecord,
        peer_node_id: NodeId,
    ) -> Result<ImportOutcome, DiscoveryBookError> {
        if record.body.node_id != peer_node_id {
            return Err(DiscoveryBookError::MismatchedConnectedPeerRecord);
        }

        let now = current_unix_secs();
        let services = normalize_services(record.body.services.clone());
        let mut inner = self.inner.lock().await;
        let context = inner.validation_context(now);
        record.verify(&context)?;
        if record.body.services.len() > inner.config.book_limits.max_services_per_record {
            return Err(DiscoveryBookError::Limit {
                field: "service",
                actual: record.body.services.len(),
                max: inner.config.book_limits.max_services_per_record,
            });
        }

        let import_result =
            inner
                .book
                .import_record(record.clone(), Some(peer_node_id), now, &context);
        if import_result.is_ok()
            || matches!(import_result, Err(ref error) if is_direct_address_import_error(error))
        {
            inner.update_active_services_from_connected_record(
                peer_node_id,
                record,
                services,
                &import_result,
            );
        }
        import_result
    }

    /// Imports records loaded from persistent storage through the validation path.
    pub async fn import_persisted_entries(
        &self,
        entries: impl IntoIterator<Item = ZakuraDiscoveryPersistedEntry>,
    ) -> ImportBatchOutcome {
        let now = current_unix_secs();
        let mut inner = self.inner.lock().await;
        let context = inner.validation_context(now);
        inner.book.import_persisted_entries(entries, now, &context)
    }

    /// Inserts a trusted static/bootstrap dial candidate.
    pub async fn insert_static_candidate(
        &self,
        node_addr: NodeAddr,
    ) -> Result<(), DiscoveryBookError> {
        let mut inner = self.inner.lock().await;
        inner
            .book
            .insert_static_candidate(node_addr, current_unix_secs())
    }

    /// Returns a bounded random sample of owned peer records.
    pub async fn sample_peers(
        &self,
        limit: usize,
        wanted_services: &[ZakuraServiceId],
        exclude_node_ids: &[NodeId],
    ) -> Vec<ZakuraNodeRecord> {
        let now = current_unix_secs();
        let inner = self.inner.lock().await;
        let wanted_services = bounded_services(wanted_services, inner.config.book_limits);
        let exclude_node_ids =
            bounded_node_ids(exclude_node_ids, inner.config.max_excluded_node_ids);
        let mut rng = rand::thread_rng();
        inner.book.sample_peers(
            limit.min(inner.config.peer_sample_limit),
            &wanted_services,
            &exclude_node_ids,
            now,
            &mut rng,
        )
    }

    /// Returns owned dial candidates, excluding peers currently registered with the supervisor.
    pub async fn dial_candidates(
        &self,
        wanted_services: &[ZakuraServiceId],
        in_flight_node_ids: &[NodeId],
    ) -> Vec<ZakuraDiscoveryDialCandidate> {
        let connected = self.connected.borrow().clone();
        let (limit, dial_backoff_base, dial_backoff_max, book_limits) = {
            let inner = self.inner.lock().await;
            (
                discovery_dial_slot_limit(
                    connected.len(),
                    in_flight_node_ids.len(),
                    inner.config.max_zakura_connections,
                    inner.config.discovery_connection_headroom,
                    inner.config.max_concurrent_discovery_dials,
                ),
                inner.config.dial_backoff_base,
                inner.config.dial_backoff_max,
                inner.config.book_limits,
            )
        };
        if limit == 0 {
            return Vec::new();
        }

        let connected_node_ids = connected_peer_node_ids(&connected);
        let now = current_unix_secs();
        let inner = self.inner.lock().await;
        let wanted_services = bounded_services(wanted_services, book_limits);
        let mut rng = rand::thread_rng();
        inner.book.dial_candidates(
            limit,
            &wanted_services,
            DialCandidateExclusions {
                connected_node_ids: &connected_node_ids,
                in_flight_node_ids,
            },
            now,
            (dial_backoff_base, dial_backoff_max),
            &mut rng,
        )
    }

    /// Returns service-aware candidates, using fallback to general peers only when requested.
    pub async fn service_candidates(
        &self,
        service: &ZakuraServiceId,
        allow_fallback: bool,
        in_flight_node_ids: &[NodeId],
    ) -> ZakuraServiceCandidates {
        let connected = self.connected.borrow().clone();
        let connected_node_ids = connected_peer_node_ids(&connected);
        let (limit, dial_backoff_base, dial_backoff_max, book_limits) = {
            let inner = self.inner.lock().await;
            (
                discovery_dial_slot_limit(
                    connected.len(),
                    in_flight_node_ids.len(),
                    inner.config.max_zakura_connections,
                    inner.config.discovery_connection_headroom,
                    inner.config.max_concurrent_discovery_dials,
                ),
                inner.config.dial_backoff_base,
                inner.config.dial_backoff_max,
                inner.config.book_limits,
            )
        };

        let now = current_unix_secs();
        let mut inner = self.inner.lock().await;
        inner.sync_active_services(&connected_node_ids, now);

        let mut connected_entries: Vec<_> = inner
            .active_services
            .iter()
            .filter(|(node_id, entry)| {
                connected_node_ids.contains(node_id) && entry.services.contains(service)
            })
            .map(|(node_id, entry)| {
                (
                    *node_id,
                    entry.live_summary_preference(service, now),
                    node_id_sort_key(node_id),
                )
            })
            .collect();
        connected_entries
            .sort_by_key(|(_, preference, sort_key)| (Reverse(*preference), *sort_key));
        let connected: Vec<NodeId> = connected_entries
            .into_iter()
            .map(|(node_id, _, _)| node_id)
            .collect();

        let wanted_services = bounded_services(std::slice::from_ref(service), book_limits);
        let mut rng = rand::thread_rng();
        let mut discovered = inner.book.dial_candidates(
            limit,
            &wanted_services,
            DialCandidateExclusions {
                connected_node_ids: &connected_node_ids,
                in_flight_node_ids,
            },
            now,
            (dial_backoff_base, dial_backoff_max),
            &mut rng,
        );
        let used_fallback = allow_fallback && connected.is_empty() && discovered.is_empty();
        if used_fallback {
            discovered = inner.book.dial_candidates(
                limit,
                &[],
                DialCandidateExclusions {
                    connected_node_ids: &connected_node_ids,
                    in_flight_node_ids,
                },
                now,
                (dial_backoff_base, dial_backoff_max),
                &mut rng,
            );
        }

        ZakuraServiceCandidates {
            connected,
            discovered,
            used_fallback,
        }
    }

    /// Returns header-sync candidates filtered and ordered by first-party advisory summaries.
    ///
    /// The returned peers are still advisory only: header-sync `Status` and header validation remain
    /// authoritative after admission.
    pub async fn header_sync_candidates(
        &self,
        header_sync: &ZakuraHeaderSyncCandidateState,
        allow_fallback: bool,
        in_flight_node_ids: &[NodeId],
    ) -> ZakuraServiceCandidates {
        let connected = self.connected.borrow().clone();
        let connected_node_ids = connected_peer_node_ids(&connected);
        let (limit, dial_backoff_base, dial_backoff_max, book_limits) = {
            let inner = self.inner.lock().await;
            (
                discovery_dial_slot_limit(
                    connected.len(),
                    in_flight_node_ids.len(),
                    inner.config.max_zakura_connections,
                    inner.config.discovery_connection_headroom,
                    inner.config.max_concurrent_discovery_dials,
                ),
                inner.config.dial_backoff_base,
                inner.config.dial_backoff_max,
                inner.config.book_limits,
            )
        };

        let now = current_unix_secs();
        let mut inner = self.inner.lock().await;
        inner.sync_active_services(&connected_node_ids, now);

        let admitted_node_ids: HashSet<_> = header_sync.admitted_node_ids.iter().copied().collect();
        let backed_off_node_ids: HashSet<_> =
            header_sync.backed_off_node_ids.iter().copied().collect();
        let header_sync_service = ZakuraServiceId::header_sync();
        let mut connected_entries: Vec<_> = inner
            .active_services
            .iter()
            .filter_map(|(node_id, entry)| {
                let is_admitted = admitted_node_ids.contains(node_id);
                if !connected_node_ids.contains(node_id)
                    || !entry.services.contains(&header_sync_service)
                    || (backed_off_node_ids.contains(node_id) && !is_admitted)
                {
                    return None;
                }

                let summary = entry.fresh_header_sync_summary(now);
                if summary.is_some_and(|summary| {
                    summary.serving_headers && summary.inbound_slots_free == 0 && !is_admitted
                }) {
                    return None;
                }

                Some((
                    *node_id,
                    header_sync_candidate_preference(summary, header_sync.target_height),
                    node_id_sort_key(node_id),
                ))
            })
            .collect();
        connected_entries
            .sort_by_key(|(_, preference, sort_key)| (Reverse(*preference), *sort_key));
        let connected: Vec<NodeId> = connected_entries
            .into_iter()
            .map(|(node_id, _, _)| node_id)
            .collect();

        let wanted_services =
            bounded_services(std::slice::from_ref(&header_sync_service), book_limits);
        let mut excluded_node_ids = Vec::with_capacity(
            in_flight_node_ids
                .len()
                .saturating_add(header_sync.backed_off_node_ids.len()),
        );
        excluded_node_ids.extend_from_slice(in_flight_node_ids);
        excluded_node_ids.extend(
            header_sync
                .backed_off_node_ids
                .iter()
                .copied()
                .filter(|node_id| !admitted_node_ids.contains(node_id)),
        );
        let mut rng = rand::thread_rng();
        let mut discovered = inner.book.dial_candidates(
            limit,
            &wanted_services,
            DialCandidateExclusions {
                connected_node_ids: &connected_node_ids,
                in_flight_node_ids: &excluded_node_ids,
            },
            now,
            (dial_backoff_base, dial_backoff_max),
            &mut rng,
        );
        let used_fallback = allow_fallback && connected.is_empty() && discovered.is_empty();
        if used_fallback {
            discovered = inner.book.dial_candidates(
                limit,
                &[],
                DialCandidateExclusions {
                    connected_node_ids: &connected_node_ids,
                    in_flight_node_ids: &excluded_node_ids,
                },
                now,
                (dial_backoff_base, dial_backoff_max),
                &mut rng,
            );
        }

        ZakuraServiceCandidates {
            connected,
            discovered,
            used_fallback,
        }
    }

    /// Returns block-sync candidates filtered and ordered by first-party advisory summaries.
    ///
    /// The returned peers are still advisory only: block-sync `Status`, body hash validation, and
    /// consensus verification remain authoritative after admission.
    pub async fn block_sync_candidates(
        &self,
        block_sync: &ZakuraBlockSyncCandidateState,
        allow_fallback: bool,
        in_flight_node_ids: &[NodeId],
    ) -> ZakuraServiceCandidates {
        let connected = self.connected.borrow().clone();
        let connected_node_ids = connected_peer_node_ids(&connected);
        let (limit, dial_backoff_base, dial_backoff_max, book_limits) = {
            let inner = self.inner.lock().await;
            (
                discovery_dial_slot_limit(
                    connected.len(),
                    in_flight_node_ids.len(),
                    inner.config.max_zakura_connections,
                    inner.config.discovery_connection_headroom,
                    inner.config.max_concurrent_discovery_dials,
                ),
                inner.config.dial_backoff_base,
                inner.config.dial_backoff_max,
                inner.config.book_limits,
            )
        };

        let now = current_unix_secs();
        let mut inner = self.inner.lock().await;
        inner.sync_active_services(&connected_node_ids, now);

        let admitted_node_ids: HashSet<_> = block_sync.admitted_node_ids.iter().copied().collect();
        let block_sync_service = ZakuraServiceId::block_sync();
        let mut connected_entries: Vec<_> = inner
            .active_services
            .iter()
            .filter_map(|(node_id, entry)| {
                if !connected_node_ids.contains(node_id)
                    || !entry.services.contains(&block_sync_service)
                {
                    return None;
                }

                if block_sync.missing_block_bodies.is_empty() {
                    return Some((
                        *node_id,
                        entry.live_summary_preference(&block_sync_service, now),
                        node_id_sort_key(node_id),
                    ));
                }

                let is_admitted = admitted_node_ids.contains(node_id);
                let summary = entry.fresh_block_sync_summary(now);
                if summary.is_some_and(|summary| summary.free_slots == 0 && !is_admitted) {
                    return None;
                }

                Some((
                    *node_id,
                    block_sync_candidate_preference(summary, &block_sync.missing_block_bodies),
                    node_id_sort_key(node_id),
                ))
            })
            .collect();
        connected_entries
            .sort_by_key(|(_, preference, sort_key)| (Reverse(*preference), *sort_key));
        let connected: Vec<NodeId> = connected_entries
            .into_iter()
            .map(|(node_id, _, _)| node_id)
            .collect();

        let wanted_services =
            bounded_services(std::slice::from_ref(&block_sync_service), book_limits);
        let mut rng = rand::thread_rng();
        let mut discovered = inner.book.dial_candidates(
            limit,
            &wanted_services,
            DialCandidateExclusions {
                connected_node_ids: &connected_node_ids,
                in_flight_node_ids,
            },
            now,
            (dial_backoff_base, dial_backoff_max),
            &mut rng,
        );
        let used_fallback = allow_fallback && connected.is_empty() && discovered.is_empty();
        if used_fallback {
            discovered = inner.book.dial_candidates(
                limit,
                &[],
                DialCandidateExclusions {
                    connected_node_ids: &connected_node_ids,
                    in_flight_node_ids,
                },
                now,
                (dial_backoff_base, dial_backoff_max),
                &mut rng,
            );
        }

        ZakuraServiceCandidates {
            connected,
            discovered,
            used_fallback,
        }
    }

    /// Marks a discovery dial attempt for `node_id`.
    pub async fn mark_dial_attempt(&self, node_id: &NodeId) {
        let mut inner = self.inner.lock().await;
        inner.book.mark_dial_attempt(node_id, current_unix_secs());
    }

    /// Marks a successful discovery dial for `node_id`.
    pub async fn mark_dial_success(&self, node_id: &NodeId) {
        let mut inner = self.inner.lock().await;
        inner.book.mark_dial_success(node_id, current_unix_secs());
    }

    /// Marks a failed discovery dial for `node_id`.
    pub async fn mark_dial_failure(&self, node_id: &NodeId) {
        let mut inner = self.inner.lock().await;
        inner.book.mark_dial_failure(node_id, current_unix_secs());
    }

    /// Marks a completed short-lived discovery exchange for local redial backoff.
    pub async fn mark_short_lived_exchange(&self, node_id: &NodeId) {
        let mut inner = self.inner.lock().await;
        inner
            .book
            .mark_short_lived_exchange(node_id, current_unix_secs());
    }

    /// Returns a connected peer's advertised services as a derived supervisor-watch projection.
    pub async fn active_services(&self, node_id: NodeId) -> Option<Vec<ZakuraServiceId>> {
        let connected = connected_peer_node_ids(&self.connected.borrow());
        let now = current_unix_secs();
        let mut inner = self.inner.lock().await;
        inner.sync_active_services(&connected, now);
        if !connected.contains(&node_id) {
            return None;
        }

        inner
            .active_services
            .get(&node_id)
            .map(|entry| entry.services.clone())
    }

    /// Returns fresh first-party live summaries cached for a connected peer.
    pub async fn live_service_summaries(
        &self,
        node_id: NodeId,
    ) -> Option<Vec<ZakuraCachedLiveServiceSummary>> {
        self.live_service_summaries_at(node_id, current_unix_secs())
            .await
    }

    async fn live_service_summaries_at(
        &self,
        node_id: NodeId,
        now: u64,
    ) -> Option<Vec<ZakuraCachedLiveServiceSummary>> {
        let connected = connected_peer_node_ids(&self.connected.borrow());
        let mut inner = self.inner.lock().await;
        inner.sync_active_services(&connected, now);
        if !connected.contains(&node_id) {
            return None;
        }

        inner
            .active_services
            .get(&node_id)
            .map(|entry| entry.live_summaries.clone())
    }

    /// Returns a stored discovery record for `node_id`, if present.
    pub async fn record_for(&self, node_id: NodeId) -> Option<ZakuraNodeRecord> {
        let inner = self.inner.lock().await;
        inner.book.get(&node_id).map(|entry| entry.record().clone())
    }

    /// Returns owned entries suitable for a future Zakura-specific persistent cache writer.
    pub async fn persisted_entries(&self) -> Vec<ZakuraDiscoveryPersistedEntry> {
        let inner = self.inner.lock().await;
        inner.book.persisted_entries()
    }
}

impl ZakuraDiscoveryInner {
    fn validation_context(&self, now: u64) -> DiscoveryRecordValidationContext {
        DiscoveryRecordValidationContext {
            expected_network_id: self.local.network_id,
            expected_chain_id: self.local.chain_id,
            current_unix_secs: now,
            supported_protocol_min: self.local.zakura_protocol_min,
            supported_protocol_max: self.local.zakura_protocol_max,
            max_record_ttl: self.config.max_record_ttl,
            clock_skew_tolerance: self.config.clock_skew_tolerance,
        }
    }

    fn sync_active_services(&mut self, connected_node_ids: &[NodeId], now_unix_secs: u64) {
        let connected_node_ids: HashSet<_> = connected_node_ids.iter().copied().collect();
        self.active_services
            .retain(|node_id, _| connected_node_ids.contains(node_id));
        for entry in self.active_services.values_mut() {
            entry.prune_expired_live_summaries(now_unix_secs);
        }
        self.active_services
            .retain(|_, entry| !entry.is_empty_live_only());
        self.last_connected_node_ids = connected_node_ids;
    }

    fn update_active_services_from_connected_record(
        &mut self,
        node_id: NodeId,
        record: ZakuraNodeRecord,
        services: Vec<ZakuraServiceId>,
        import_result: &Result<ImportOutcome, DiscoveryBookError>,
    ) {
        if self.active_services.get(&node_id).is_some_and(|active| {
            active
                .record
                .as_ref()
                .is_some_and(|active_record| record.body.sequence <= active_record.body.sequence)
        }) {
            return;
        }

        let should_update = match import_result {
            Ok(ImportOutcome::Added | ImportOutcome::Updated) => true,
            Ok(ImportOutcome::MetadataUpdated) => self
                .book
                .get(&node_id)
                .is_some_and(|entry| entry.record() == &record),
            Ok(ImportOutcome::IgnoredOlder) => false,
            Err(error) if is_direct_address_import_error(error) => self
                .book
                .get(&node_id)
                .is_none_or(|entry| connected_record_is_fresh_for_stored_record(&record, entry)),
            Err(_) => false,
        };

        if should_update {
            self.active_services
                .entry(node_id)
                .and_modify(|entry| entry.update_record(record.clone(), services.clone()))
                .or_insert_with(|| ZakuraActiveServiceEntry::from_record(record, services));
        }
    }
}

impl ZakuraLocalDiscoveryState {
    fn new(config: ZakuraDiscoveryLocalConfig, wall_clock_sequence: u64) -> Self {
        let node_id = config.secret_key.public();
        let persisted_next_sequence = config
            .last_authored_sequence
            .map(|sequence| sequence.saturating_add(1))
            .unwrap_or(0);
        Self {
            secret_key: config.secret_key,
            node_id,
            direct_addrs: normalize_direct_addrs(config.direct_addrs),
            services: normalize_services(config.services),
            zakura_protocol_min: config.zakura_protocol_min,
            zakura_protocol_max: config.zakura_protocol_max,
            network_id: config.network_id,
            chain_id: config.chain_id,
            next_sequence: persisted_next_sequence.max(wall_clock_sequence),
        }
    }

    fn build_self_record(
        &mut self,
        now_unix_secs: u64,
        wall_clock_sequence: u64,
        record_ttl: Duration,
    ) -> Result<ZakuraNodeRecord, DiscoveryWireError> {
        let sequence = self.next_sequence.max(wall_clock_sequence);
        self.next_sequence = sequence.saturating_add(1);
        ZakuraNodeRecord::sign(
            ZakuraNodeRecordBody {
                node_id: self.node_id,
                direct_addrs: self.direct_addrs.clone(),
                services: self.services.clone(),
                zakura_protocol_min: self.zakura_protocol_min,
                zakura_protocol_max: self.zakura_protocol_max,
                network_id: self.network_id,
                chain_id: self.chain_id,
                sequence,
                expires_at_unix_secs: now_unix_secs.saturating_add(record_ttl.as_secs()),
            },
            &self.secret_key,
        )
    }
}

impl Default for ZakuraDiscoveryBook {
    fn default() -> Self {
        Self::new(ZakuraDiscoveryBookLimits::default())
    }
}

impl ZakuraDiscoveryBook {
    /// Creates an empty discovery book with `limits`.
    pub fn new(limits: ZakuraDiscoveryBookLimits) -> Self {
        Self {
            entries: HashMap::new(),
            static_candidates: HashMap::new(),
            limits,
            local_node_id: None,
        }
    }

    /// Creates an empty discovery book that rejects the local node's record.
    pub fn with_local_node_id(limits: ZakuraDiscoveryBookLimits, local_node_id: NodeId) -> Self {
        Self {
            entries: HashMap::new(),
            static_candidates: HashMap::new(),
            limits,
            local_node_id: Some(local_node_id),
        }
    }

    /// Returns the storage limits.
    pub fn limits(&self) -> ZakuraDiscoveryBookLimits {
        self.limits
    }

    /// Returns the total number of stored records, including static records.
    pub fn len(&self) -> usize {
        self.entries.len()
            + self
                .static_candidates
                .keys()
                .filter(|node_id| !self.entries.contains_key(*node_id))
                .count()
    }

    /// Returns true when the book has no records.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Returns the number of non-static records learned from peers.
    pub fn discovered_len(&self) -> usize {
        self.entries
            .values()
            .filter(|entry| !entry.is_static)
            .count()
    }

    /// Returns the entry for `node_id`, if present.
    pub fn get(&self, node_id: &NodeId) -> Option<&ZakuraDiscoveryEntry> {
        self.entries.get(node_id)
    }

    /// Inserts or refreshes one trusted static/bootstrap dial candidate.
    ///
    /// Configured bootstrap peers are not signed discovery records, so they are stored separately
    /// from peer-supplied records and are never returned in peer samples.
    pub fn insert_static_candidate(
        &mut self,
        node_addr: NodeAddr,
        now: u64,
    ) -> Result<(), DiscoveryBookError> {
        if self.local_node_id == Some(node_addr.node_id) {
            return Err(DiscoveryBookError::SelfRecord);
        }

        let mut direct_addrs: Vec<_> = node_addr.direct_addresses().copied().collect();
        direct_addrs.sort_unstable();
        direct_addrs.dedup();
        if direct_addrs.is_empty() {
            return Err(DiscoveryBookError::NoUsableDirectAddress);
        }
        for addr in &direct_addrs {
            if !is_static_discovery_configured_addr_usable(addr) {
                return Err(DiscoveryBookError::NonDialableDirectAddress { addr: *addr });
            }
        }

        let candidate = self
            .static_candidates
            .entry(node_addr.node_id)
            .or_insert_with(|| ZakuraStaticDiscoveryCandidate {
                node_id: node_addr.node_id,
                direct_addrs: Vec::new(),
                last_seen: now,
                last_dial_attempt: None,
                last_success: None,
                last_short_lived_exchange: None,
                failure_count: 0,
            });
        candidate.direct_addrs = direct_addrs;
        candidate.last_seen = now;
        Ok(())
    }

    /// Imports one signed peer record from a discovery source.
    pub fn import_record(
        &mut self,
        record: ZakuraNodeRecord,
        source: Option<NodeId>,
        now: u64,
        context: &DiscoveryRecordValidationContext,
    ) -> Result<ImportOutcome, DiscoveryBookError> {
        self.import_record_inner(record, source, false, false, now, context)
    }

    /// Imports one signed static/bootstrap record.
    ///
    /// Static records are trusted-by-operator configuration, so this import path permits loopback
    /// and other local direct addresses needed by single-host regtest deployments. Untrusted
    /// peer/gossip imports through [`Self::import_record`] keep requiring globally dialable
    /// addresses.
    pub fn import_static_record(
        &mut self,
        record: ZakuraNodeRecord,
        now: u64,
        context: &DiscoveryRecordValidationContext,
    ) -> Result<ImportOutcome, DiscoveryBookError> {
        self.import_record_inner(record, None, true, false, now, context)
    }

    /// Imports a batch of records whose signatures were already verified outside the lock.
    ///
    /// [`ZakuraDiscoveryHandle::import_peer_records`] performs signature verification and
    /// applies the per-response cap before taking the global discovery mutex, so this only
    /// runs the cheap storage-limit recheck, address-policy check, map mutation, and
    /// eviction under the lock. `attempted`/`dropped_for_limit` are owned by the caller;
    /// this updates the per-record success and rejection tallies on `outcome`.
    fn import_pre_verified_records(
        &mut self,
        records: impl IntoIterator<Item = ZakuraNodeRecord>,
        source: Option<NodeId>,
        now: u64,
        context: &DiscoveryRecordValidationContext,
        outcome: &mut ImportBatchOutcome,
    ) {
        for record in records {
            match self.import_record_inner(record, source, false, true, now, context) {
                Ok(import_outcome) => outcome.record_success(import_outcome),
                Err(_) => outcome.rejected += 1,
            }
        }
    }

    /// Imports a bounded batch of signed peer records from a single response.
    pub fn import_records(
        &mut self,
        records: impl IntoIterator<Item = ZakuraNodeRecord>,
        source: Option<NodeId>,
        now: u64,
        context: &DiscoveryRecordValidationContext,
    ) -> ImportBatchOutcome {
        let mut outcome = ImportBatchOutcome::default();
        for (index, record) in records.into_iter().enumerate() {
            if index >= self.limits.max_imported_records_per_response {
                outcome.dropped_for_limit += 1;
                continue;
            }
            outcome.attempted += 1;
            match self.import_record(record, source, now, context) {
                Ok(import_outcome) => outcome.record_success(import_outcome),
                Err(_) => outcome.rejected += 1,
            }
        }
        outcome
    }

    /// Returns a bounded random peer sample.
    pub fn sample_peers<R: rand::Rng + ?Sized>(
        &self,
        limit: usize,
        wanted_services: &[ZakuraServiceId],
        exclude_node_ids: &[NodeId],
        now: u64,
        rng: &mut R,
    ) -> Vec<ZakuraNodeRecord> {
        let exclude_node_ids: HashSet<_> = exclude_node_ids.iter().copied().collect();
        let limit = limit.min(self.limits.max_imported_records_per_response);

        // Reservoir-sample references and clone only the chosen records. The book can hold
        // `max_records` (default 10_000) entries, each up to `max_encoded_record_bytes`, so cloning
        // every record that passes the filter — when at most `limit` (default 32) are ever returned
        // — was attacker-paced, allocation-heavy work performed while holding the global discovery
        // mutex. Per-call clone work is now bounded by `limit`, not the book size. See finding
        // `claude-discovery-expensive-work-under-global-mutex` (SR-2/SR-4).
        self.entries
            .iter()
            .filter(|(node_id, entry)| {
                !exclude_node_ids.contains(*node_id)
                    && self.local_node_id != Some(**node_id)
                    && !entry_is_expired(entry, now)
                    && has_wanted_services(&entry.record, wanted_services)
                    && has_discovery_dialable_direct_addrs(&entry.record)
            })
            .map(|(_, entry)| &entry.record)
            .choose_multiple(rng, limit)
            .into_iter()
            .cloned()
            .collect()
    }

    /// Returns bounded dial candidates for later dial-loop code.
    pub(crate) fn dial_candidates<R: rand::Rng + ?Sized>(
        &self,
        limit: usize,
        wanted_services: &[ZakuraServiceId],
        exclusions: DialCandidateExclusions<'_>,
        now: u64,
        dial_backoff: (Duration, Duration),
        rng: &mut R,
    ) -> Vec<ZakuraDiscoveryDialCandidate> {
        let connected_node_ids: HashSet<_> =
            exclusions.connected_node_ids.iter().copied().collect();
        let in_flight_node_ids: HashSet<_> =
            exclusions.in_flight_node_ids.iter().copied().collect();
        let candidates: Vec<_> = self
            .entries
            .values()
            .filter(|entry| {
                !connected_node_ids.contains(&entry.record.body.node_id)
                    && !in_flight_node_ids.contains(&entry.record.body.node_id)
                    && self.local_node_id != Some(entry.record.body.node_id)
                    && !entry_is_expired(entry, now)
                    && !entry_in_dial_backoff(entry, now, dial_backoff.0, dial_backoff.1)
                    && !entry_in_short_lived_exchange_backoff(
                        entry,
                        now,
                        dial_backoff.0,
                        dial_backoff.1,
                    )
                    && entry_has_confirmed_dial_authority(entry)
                    && has_wanted_services(&entry.record, wanted_services)
                    && has_discovery_usable_direct_addrs(entry)
            })
            .map(DialCandidateRef::SignedRecord)
            .chain(self.static_candidates.values().filter_map(|candidate| {
                if !wanted_services.is_empty()
                    || connected_node_ids.contains(&candidate.node_id)
                    || in_flight_node_ids.contains(&candidate.node_id)
                    || self.local_node_id == Some(candidate.node_id)
                    || self.entries.contains_key(&candidate.node_id)
                    || entry_metadata_in_dial_backoff(
                        candidate.last_dial_attempt,
                        candidate.failure_count,
                        now,
                        dial_backoff.0,
                        dial_backoff.1,
                    )
                    || entry_metadata_in_short_lived_exchange_backoff(
                        candidate.last_short_lived_exchange,
                        now,
                        dial_backoff.0,
                        dial_backoff.1,
                    )
                {
                    return None;
                }
                Some(DialCandidateRef::StaticConfigured(candidate))
            }))
            .collect();

        // Bounded top-k selection instead of a full sort. The book can hold `max_records`
        // (default 10_000) entries and this selection runs under the global discovery mutex on the
        // per-second candidate-dialer path, yet only `limit` candidates are ever returned. Sorting
        // the whole candidate set (O(n log n)) just to `take(limit)` is replaced with an O(n)
        // partial select of the best `limit` candidates plus an O(limit log limit) sort of the
        // survivors, keeping the order identical. See finding
        // `claude-discovery-expensive-work-under-global-mutex` (SR-2/SR-4).
        let mut keyed: Vec<(DialCandidateSortKey, DialCandidateRef<'_>)> = candidates
            .into_iter()
            .map(|candidate| (dial_candidate_sort_key(&candidate, rng), candidate))
            .collect();

        let take = limit.min(keyed.len());
        if take < keyed.len() {
            keyed.select_nth_unstable_by(take, |a, b| a.0.cmp(&b.0));
            keyed.truncate(take);
        }
        keyed.sort_by_key(|a| a.0);

        keyed
            .into_iter()
            .map(|(_, candidate)| candidate.into_candidate())
            .collect()
    }

    /// Marks a dial attempt for `node_id`.
    pub fn mark_dial_attempt(&mut self, node_id: &NodeId, now: u64) {
        if let Some(entry) = self.entries.get_mut(node_id) {
            entry.last_dial_attempt = Some(now);
        }
        if let Some(candidate) = self.static_candidates.get_mut(node_id) {
            candidate.last_dial_attempt = Some(now);
        }
    }

    /// Marks a successful dial for `node_id`.
    pub fn mark_dial_success(&mut self, node_id: &NodeId, now: u64) {
        if let Some(entry) = self.entries.get_mut(node_id) {
            entry.last_success = Some(now);
            entry.failure_count = 0;
        }
        if let Some(candidate) = self.static_candidates.get_mut(node_id) {
            candidate.last_success = Some(now);
            candidate.failure_count = 0;
        }
    }

    /// Marks a failed dial for `node_id` without blacklisting it.
    pub fn mark_dial_failure(&mut self, node_id: &NodeId, _now: u64) {
        if let Some(entry) = self.entries.get_mut(node_id) {
            entry.failure_count = entry.failure_count.saturating_add(1);
        }
        if let Some(candidate) = self.static_candidates.get_mut(node_id) {
            candidate.failure_count = candidate.failure_count.saturating_add(1);
        }
    }

    /// Marks a successful short-lived discovery exchange for `node_id`.
    pub fn mark_short_lived_exchange(&mut self, node_id: &NodeId, now: u64) {
        if let Some(entry) = self.entries.get_mut(node_id) {
            entry.last_short_lived_exchange = Some(now);
        }
        if let Some(candidate) = self.static_candidates.get_mut(node_id) {
            candidate.last_short_lived_exchange = Some(now);
        }
    }

    /// Returns the advertised services for `node_id`.
    pub fn services_for(&self, node_id: &NodeId) -> Option<Vec<ZakuraServiceId>> {
        self.entries
            .get(node_id)
            .map(|entry| entry.record.body.services.clone())
    }

    fn recent_node_ids(&self, limit: usize) -> Vec<NodeId> {
        let mut entries: Vec<_> = self.entries.values().collect();
        entries.sort_by_key(|entry| {
            (
                Reverse(entry.last_seen),
                node_id_sort_key(&entry.record.body.node_id),
            )
        });
        entries
            .into_iter()
            .take(limit)
            .map(|entry| entry.record.body.node_id)
            .collect()
    }

    /// Returns entries suitable for a Zakura-specific persistent cache.
    pub fn persisted_entries(&self) -> Vec<ZakuraDiscoveryPersistedEntry> {
        let mut entries: Vec<_> = self
            .entries
            .values()
            .map(|entry| ZakuraDiscoveryPersistedEntry {
                record: entry.record.clone(),
                source: entry.source,
                is_static: entry.is_static,
                last_seen: entry.last_seen,
                last_dial_attempt: entry.last_dial_attempt,
                last_success: entry.last_success,
                failure_count: entry.failure_count,
            })
            .collect();
        entries.sort_by_key(|entry| node_id_sort_key(&entry.record.body.node_id));
        entries
    }

    /// Re-validates and imports entries loaded from a persistent cache.
    pub fn import_persisted_entries(
        &mut self,
        entries: impl IntoIterator<Item = ZakuraDiscoveryPersistedEntry>,
        now: u64,
        context: &DiscoveryRecordValidationContext,
    ) -> ImportBatchOutcome {
        let mut outcome = ImportBatchOutcome::default();
        for entry in entries {
            outcome.attempted += 1;
            match self.import_persisted_entry(entry, now, context) {
                Ok(import_outcome) => outcome.record_success(import_outcome),
                Err(_) => outcome.rejected += 1,
            }
        }
        outcome
    }

    fn import_persisted_entry(
        &mut self,
        entry: ZakuraDiscoveryPersistedEntry,
        now: u64,
        context: &DiscoveryRecordValidationContext,
    ) -> Result<ImportOutcome, DiscoveryBookError> {
        let metadata = DiscoveryEntryMetadata {
            source: entry.source,
            is_static: entry.is_static,
            last_seen: entry.last_seen,
            last_dial_attempt: entry.last_dial_attempt,
            last_success: entry.last_success,
            failure_count: entry.failure_count,
        };
        self.import_validated_record(entry.record, metadata, false, now, context)
    }

    fn import_record_inner(
        &mut self,
        record: ZakuraNodeRecord,
        source: Option<NodeId>,
        is_static: bool,
        pre_verified: bool,
        now: u64,
        context: &DiscoveryRecordValidationContext,
    ) -> Result<ImportOutcome, DiscoveryBookError> {
        let metadata = DiscoveryEntryMetadata {
            source,
            is_static,
            last_seen: now,
            last_dial_attempt: None,
            last_success: None,
            failure_count: 0,
        };
        self.import_validated_record(record, metadata, pre_verified, now, context)
    }

    /// Validates and stores one record.
    ///
    /// When `pre_verified` is set the signature/import-context check has already been run
    /// outside the global discovery mutex by [`ZakuraDiscoveryHandle::import_peer_records`],
    /// so it is not repeated here; the cheap storage-limit, address-policy, insertion, and
    /// eviction steps still run under the caller's lock.
    fn import_validated_record(
        &mut self,
        record: ZakuraNodeRecord,
        metadata: DiscoveryEntryMetadata,
        pre_verified: bool,
        now: u64,
        context: &DiscoveryRecordValidationContext,
    ) -> Result<ImportOutcome, DiscoveryBookError> {
        if self.local_node_id == Some(record.body.node_id) {
            return Err(DiscoveryBookError::SelfRecord);
        }

        if !pre_verified {
            record.verify(context)?;
        }
        self.validate_record_storage_limits(&record)?;
        let direct_addr_policy = if metadata.is_static {
            DiscoveryDirectAddrPolicy::StaticConfigured
        } else {
            DiscoveryDirectAddrPolicy::UntrustedPeer
        };
        validate_discovery_direct_addrs(&record, direct_addr_policy)?;

        let node_id = record.body.node_id;
        if let Some(entry) = self.entries.get_mut(&node_id) {
            return Ok(update_existing_entry(entry, record, metadata));
        }

        self.entries.insert(
            node_id,
            ZakuraDiscoveryEntry {
                record,
                source: metadata.source,
                is_static: metadata.is_static,
                last_seen: metadata.last_seen,
                last_dial_attempt: metadata.last_dial_attempt,
                last_success: metadata.last_success,
                last_short_lived_exchange: None,
                failure_count: metadata.failure_count,
            },
        );
        self.evict_to_limits(now);
        Ok(ImportOutcome::Added)
    }

    fn validate_record_storage_limits(
        &self,
        record: &ZakuraNodeRecord,
    ) -> Result<(), DiscoveryBookError> {
        if record.body.direct_addrs.len() > self.limits.max_direct_addrs_per_record {
            return Err(DiscoveryBookError::Limit {
                field: "direct address",
                actual: record.body.direct_addrs.len(),
                max: self.limits.max_direct_addrs_per_record,
            });
        }
        if record.body.services.len() > self.limits.max_services_per_record {
            return Err(DiscoveryBookError::Limit {
                field: "service",
                actual: record.body.services.len(),
                max: self.limits.max_services_per_record,
            });
        }

        let mut encoded = Vec::new();
        encode_record(record, &mut encoded).map_err(DiscoveryRecordError::from)?;
        if encoded.len() > self.limits.max_encoded_record_bytes {
            return Err(DiscoveryBookError::Limit {
                field: "encoded record byte",
                actual: encoded.len(),
                max: self.limits.max_encoded_record_bytes,
            });
        }

        Ok(())
    }

    fn evict_to_limits(&mut self, now: u64) {
        while self.discovered_len() > self.limits.max_records {
            let Some(node_id) = self.next_eviction_candidate(now) else {
                break;
            };
            self.entries.remove(&node_id);
        }
    }

    fn next_eviction_candidate(&self, now: u64) -> Option<NodeId> {
        self.entries
            .iter()
            .filter(|(_, entry)| !entry.is_static && entry_is_expired(entry, now))
            .min_by_key(|(node_id, entry)| {
                (
                    entry.record.body.expires_at_unix_secs,
                    node_id_sort_key(node_id),
                )
            })
            .map(|(node_id, _)| *node_id)
            .or_else(|| {
                self.entries
                    .iter()
                    .filter(|(_, entry)| {
                        !entry.is_static
                            && entry.last_success.is_none()
                            && entry.failure_count >= HIGH_DIAL_FAILURE_COUNT
                    })
                    .max_by_key(|(node_id, entry)| {
                        (
                            entry.failure_count,
                            Reverse(entry.last_seen),
                            Reverse(node_id_sort_key(node_id)),
                        )
                    })
                    .map(|(node_id, _)| *node_id)
            })
            .or_else(|| {
                self.entries
                    .iter()
                    .filter(|(_, entry)| !entry.is_static)
                    .min_by_key(|(node_id, entry)| {
                        (
                            entry.last_success.unwrap_or(0),
                            entry.last_seen,
                            node_id_sort_key(node_id),
                        )
                    })
                    .map(|(node_id, _)| *node_id)
            })
    }
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
struct DiscoveryEntryMetadata {
    source: Option<NodeId>,
    is_static: bool,
    last_seen: u64,
    last_dial_attempt: Option<u64>,
    last_success: Option<u64>,
    failure_count: u32,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ZakuraStaticDiscoveryCandidate {
    node_id: NodeId,
    direct_addrs: Vec<SocketAddr>,
    last_seen: u64,
    last_dial_attempt: Option<u64>,
    last_success: Option<u64>,
    last_short_lived_exchange: Option<u64>,
    failure_count: u32,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) struct DialCandidateExclusions<'a> {
    connected_node_ids: &'a [NodeId],
    in_flight_node_ids: &'a [NodeId],
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum DiscoveryDirectAddrPolicy {
    UntrustedPeer,
    StaticConfigured,
}

enum DialCandidateRef<'a> {
    SignedRecord(&'a ZakuraDiscoveryEntry),
    StaticConfigured(&'a ZakuraStaticDiscoveryCandidate),
}

impl DialCandidateRef<'_> {
    fn node_id(&self) -> NodeId {
        match self {
            Self::SignedRecord(entry) => entry.record.body.node_id,
            Self::StaticConfigured(candidate) => candidate.node_id,
        }
    }

    fn is_static(&self) -> bool {
        match self {
            Self::SignedRecord(entry) => entry.is_static,
            Self::StaticConfigured(_) => true,
        }
    }

    fn last_seen(&self) -> u64 {
        match self {
            Self::SignedRecord(entry) => entry.last_seen,
            Self::StaticConfigured(candidate) => candidate.last_seen,
        }
    }

    fn last_success(&self) -> Option<u64> {
        match self {
            Self::SignedRecord(entry) => entry.last_success,
            Self::StaticConfigured(candidate) => candidate.last_success,
        }
    }

    fn failure_count(&self) -> u32 {
        match self {
            Self::SignedRecord(entry) => entry.failure_count,
            Self::StaticConfigured(candidate) => candidate.failure_count,
        }
    }

    fn into_candidate(self) -> ZakuraDiscoveryDialCandidate {
        match self {
            Self::SignedRecord(entry) => ZakuraDiscoveryDialCandidate {
                node_id: entry.record.body.node_id,
                direct_addrs: entry.record.body.direct_addrs.clone(),
                is_static: entry.is_static,
            },
            Self::StaticConfigured(candidate) => ZakuraDiscoveryDialCandidate {
                node_id: candidate.node_id,
                direct_addrs: candidate.direct_addrs.clone(),
                is_static: true,
            },
        }
    }
}

/// Total dial-priority key for one candidate: prefer signed records over static ones, then most
/// recent success, fewest failures, most recently seen, with a per-call random tie-break for
/// non-static candidates and a deterministic node-id tie-break for static ones.
type DialCandidateSortKey = (
    bool,
    Reverse<u64>,
    u32,
    Reverse<u64>,
    u64,
    [u8; NODE_ID_BYTES],
);

/// Computes the dial-priority key used to select the best dial candidates.
///
/// Factored out of [`ZakuraDiscoveryBook::dial_candidates`] so the key can be computed once per
/// candidate and fed to a bounded top-k selection instead of a full sort, which keeps expensive
/// per-book ordering work bounded under the global discovery mutex (finding
/// `claude-discovery-expensive-work-under-global-mutex`).
fn dial_candidate_sort_key<R: rand::Rng + ?Sized>(
    candidate: &DialCandidateRef<'_>,
    rng: &mut R,
) -> DialCandidateSortKey {
    let non_static_random_tie = if !candidate.is_static() {
        rng.gen::<u64>()
    } else {
        0
    };
    let static_deterministic_tie = if candidate.is_static() {
        node_id_sort_key(&candidate.node_id())
    } else {
        [0; NODE_ID_BYTES]
    };
    (
        !candidate.is_static(),
        Reverse(candidate.last_success().unwrap_or(0)),
        candidate.failure_count(),
        Reverse(candidate.last_seen()),
        non_static_random_tie,
        static_deterministic_tie,
    )
}

fn update_existing_entry(
    entry: &mut ZakuraDiscoveryEntry,
    record: ZakuraNodeRecord,
    metadata: DiscoveryEntryMetadata,
) -> ImportOutcome {
    let incoming_sequence = record.body.sequence;
    let stored_sequence = entry.record.body.sequence;

    if incoming_sequence < stored_sequence {
        return ImportOutcome::IgnoredOlder;
    }

    let preserve_first_party_source = incoming_sequence == stored_sequence
        && entry.source == Some(entry.record.body.node_id)
        && metadata.source != Some(entry.record.body.node_id);

    if !preserve_first_party_source {
        entry.source = metadata.source;
    }
    entry.is_static |= metadata.is_static;
    entry.last_seen = metadata.last_seen;

    if incoming_sequence == stored_sequence {
        return ImportOutcome::MetadataUpdated;
    }

    entry.record = record;
    ImportOutcome::Updated
}

fn connected_record_is_fresh_for_stored_record(
    record: &ZakuraNodeRecord,
    entry: &ZakuraDiscoveryEntry,
) -> bool {
    match record.body.sequence.cmp(&entry.record.body.sequence) {
        std::cmp::Ordering::Less => false,
        std::cmp::Ordering::Equal => entry.record() == record,
        std::cmp::Ordering::Greater => true,
    }
}

fn is_direct_address_import_error(error: &DiscoveryBookError) -> bool {
    matches!(
        error,
        DiscoveryBookError::NoUsableDirectAddress
            | DiscoveryBookError::NonDialableDirectAddress { .. }
    )
}

fn current_unix_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

/// Interim wall-clock-nanosecond self-record sequence seed.
///
/// This strictly increases across a same-key restart only while the system clock is monotonic. A
/// backward clock step can still produce a non-increasing sequence; the durable fix is to persist
/// and reload `last_authored_sequence` in the cache-persistence task.
fn current_sequence_tick() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| u64::try_from(duration.as_nanos()).unwrap_or(u64::MAX))
        .unwrap_or(0)
}

fn normalize_direct_addrs(addrs: Vec<SocketAddr>) -> Vec<SocketAddr> {
    let mut addrs: Vec<_> = addrs
        .into_iter()
        .filter(is_discovery_dialable_addr)
        .collect();
    addrs.sort_unstable();
    addrs.dedup();
    addrs.truncate(MAX_DIRECT_ADDRS_PER_RECORD);
    addrs
}

fn normalize_services(services: Vec<ZakuraServiceId>) -> Vec<ZakuraServiceId> {
    let mut services = services;
    services.sort_unstable();
    services.dedup();
    services.truncate(MAX_SERVICES_PER_RECORD);
    services
}

fn push_unique_service(services: &mut Vec<ZakuraServiceId>, service: ZakuraServiceId) {
    if services.iter().any(|existing| existing == &service) {
        return;
    }
    if services.len() < MAX_SERVICES_PER_RECORD {
        services.push(service);
        services.sort_unstable();
    }
}

fn effective_live_summary_expiry(declared_expires_at: u64, now_unix_secs: u64) -> Option<u64> {
    if declared_expires_at <= now_unix_secs {
        return None;
    }
    Some(
        declared_expires_at
            .min(now_unix_secs.saturating_add(DEFAULT_LIVE_SERVICE_SUMMARY_TTL.as_secs())),
    )
}

fn header_sync_candidate_preference(
    summary: Option<HeaderSyncServiceSummary>,
    target_height: block::Height,
) -> u32 {
    let Some(summary) = summary else {
        return 0;
    };
    if !summary.serving_headers
        || summary.inbound_slots_free == 0
        || summary.best_height < target_height
    {
        return 0;
    }

    let useful_height_delta = summary.best_height.0.saturating_sub(target_height.0);
    1_000_000u32
        .saturating_add(u32::from(summary.inbound_slots_free))
        .saturating_add(u32::from(summary.outbound_slots_free))
        .saturating_add(useful_height_delta)
}

fn block_sync_candidate_preference(
    summary: Option<BlockSyncServiceSummary>,
    missing_block_bodies: &[block::Height],
) -> u32 {
    let Some(summary) = summary else {
        return 0;
    };

    let coverage_bonus = match summary.gap_coverage(missing_block_bodies) {
        BlockSyncGapCoverage::Whole => 1_000_000u32,
        BlockSyncGapCoverage::LowPrefix => 500_000u32,
        BlockSyncGapCoverage::None => return 0,
    };

    coverage_bonus
        .saturating_add(u32::from(summary.free_slots))
        .saturating_add(
            summary
                .max_blocks_per_response
                .min(MAX_BS_BLOCKS_PER_REQUEST),
        )
}

fn bounded_services(
    services: &[ZakuraServiceId],
    limits: ZakuraDiscoveryBookLimits,
) -> Vec<ZakuraServiceId> {
    services
        .iter()
        .take(limits.max_services_per_record)
        .cloned()
        .collect()
}

fn bounded_node_ids(node_ids: &[NodeId], max_node_ids: usize) -> Vec<NodeId> {
    node_ids.iter().take(max_node_ids).copied().collect()
}

fn push_excluded_node_id(excluded: &mut Vec<NodeId>, node_id: NodeId, max_node_ids: usize) {
    if excluded.len() < max_node_ids && !excluded.contains(&node_id) {
        excluded.push(node_id);
    }
}

fn connected_peer_node_ids(connected: &[ZakuraPeerId]) -> Vec<NodeId> {
    connected
        .iter()
        .filter_map(|peer_id| {
            let bytes: [u8; NODE_ID_BYTES] = peer_id.as_bytes().try_into().ok()?;
            NodeId::from_bytes(&bytes).ok()
        })
        .collect()
}

fn discovery_dial_slot_limit(
    connected_count: usize,
    in_flight_count: usize,
    max_connections: usize,
    connection_headroom: usize,
    max_concurrent_dials: usize,
) -> usize {
    let soft_cap = max_connections.saturating_sub(connection_headroom);
    let available_connection_slots = soft_cap.saturating_sub(connected_count);
    let available_dial_slots = max_concurrent_dials.saturating_sub(in_flight_count);
    available_connection_slots.min(available_dial_slots)
}

fn has_wanted_services(record: &ZakuraNodeRecord, wanted_services: &[ZakuraServiceId]) -> bool {
    wanted_services
        .iter()
        .all(|wanted| record.body.services.iter().any(|service| service == wanted))
}

/// Validates direct addresses for discovery-book storage.
///
/// Untrusted peer/gossip records must only contain globally routable addresses, preserving the
/// discovery security rule that gossiped records cannot inject loopback, link-local, multicast,
/// broadcast, RFC 1918 private, RFC 6598 shared (CGNAT), or RFC 4193 unique-local targets. Static
/// records are trusted-by-configuration bootstrap records, so they may use
/// local addresses for regtest and single-host deployments, but still reject empty, unspecified, and
/// port-0 targets that cannot be dialed as configured peers.
fn validate_discovery_direct_addrs(
    record: &ZakuraNodeRecord,
    policy: DiscoveryDirectAddrPolicy,
) -> Result<(), DiscoveryBookError> {
    if record.body.direct_addrs.is_empty() {
        return Err(DiscoveryBookError::NoUsableDirectAddress);
    }

    for addr in &record.body.direct_addrs {
        let is_valid = match policy {
            DiscoveryDirectAddrPolicy::UntrustedPeer => is_discovery_dialable_addr(addr),
            DiscoveryDirectAddrPolicy::StaticConfigured => {
                is_static_discovery_configured_addr_usable(addr)
            }
        };
        if !is_valid {
            return Err(DiscoveryBookError::NonDialableDirectAddress { addr: *addr });
        }
    }

    Ok(())
}

fn has_discovery_dialable_direct_addrs(record: &ZakuraNodeRecord) -> bool {
    !record.body.direct_addrs.is_empty()
        && record
            .body
            .direct_addrs
            .iter()
            .all(is_discovery_dialable_addr)
}

fn has_discovery_usable_direct_addrs(entry: &ZakuraDiscoveryEntry) -> bool {
    !entry.record.body.direct_addrs.is_empty()
        && entry.record.body.direct_addrs.iter().all(|addr| {
            if entry.is_static {
                is_static_discovery_configured_addr_usable(addr)
            } else {
                is_discovery_dialable_addr(addr)
            }
        })
}

fn entry_has_confirmed_dial_authority(entry: &ZakuraDiscoveryEntry) -> bool {
    entry.is_static || entry.source == Some(entry.record.body.node_id)
}

/// Returns true only for addresses that are safe to dial from untrusted discovery gossip.
///
/// A signed `ZakuraNodeRecord` proves control of the node key, not ownership of the advertised
/// `direct_addrs`. To stop an authenticated discovery peer from steering the candidate dialer at
/// arbitrary non-public targets, gossiped records must advertise only globally routable addresses.
/// Besides the obvious loopback/link-local/multicast/broadcast cases, this rejects RFC 1918 private,
/// RFC 6598 shared (CGNAT), and RFC 4193 unique-local ranges, which are reachable internal targets
/// the dialer would otherwise scan on the operator's network.
fn is_discovery_dialable_addr(addr: &SocketAddr) -> bool {
    if addr.port() == 0 {
        return false;
    }

    match addr.ip() {
        IpAddr::V4(ip) => {
            !ip.is_unspecified()
                && !ip.is_loopback()
                && !ip.is_multicast()
                && !ip.is_broadcast()
                && !ip.is_link_local()
                && !ip.is_private()
                && !is_ipv4_shared(&ip)
        }
        IpAddr::V6(ip) => {
            !ip.is_unspecified()
                && !ip.is_loopback()
                && !ip.is_multicast()
                && !is_ipv6_unicast_link_local(&ip)
                && !is_ipv6_unique_local(&ip)
        }
    }
}

fn is_static_discovery_configured_addr_usable(addr: &SocketAddr) -> bool {
    if addr.port() == 0 {
        return false;
    }

    match addr.ip() {
        IpAddr::V4(ip) => !ip.is_unspecified() && !ip.is_multicast() && !ip.is_broadcast(),
        IpAddr::V6(ip) => !ip.is_unspecified() && !ip.is_multicast(),
    }
}

fn is_ipv6_unicast_link_local(ip: &Ipv6Addr) -> bool {
    (ip.segments()[0] & 0xffc0) == 0xfe80
}

// `Ipv4Addr::is_shared` is still unstable, so match the RFC 6598 100.64.0.0/10 range directly,
// mirroring `is_ipv6_unicast_link_local`.
fn is_ipv4_shared(ip: &Ipv4Addr) -> bool {
    let octets = ip.octets();
    octets[0] == 100 && (octets[1] & 0b1100_0000) == 0b0100_0000
}

// `Ipv6Addr::is_unique_local` is still unstable, so match the RFC 4193 fc00::/7 range directly.
fn is_ipv6_unique_local(ip: &Ipv6Addr) -> bool {
    (ip.segments()[0] & 0xfe00) == 0xfc00
}

// Import accepts records inside the clock-skew window, but runtime liveness is strict.
fn entry_is_expired(entry: &ZakuraDiscoveryEntry, now: u64) -> bool {
    entry.record.body.expires_at_unix_secs < now
}

fn entry_in_dial_backoff(
    entry: &ZakuraDiscoveryEntry,
    now: u64,
    dial_backoff_base: Duration,
    dial_backoff_max: Duration,
) -> bool {
    entry_metadata_in_dial_backoff(
        entry.last_dial_attempt,
        entry.failure_count,
        now,
        dial_backoff_base,
        dial_backoff_max,
    )
}

fn entry_metadata_in_dial_backoff(
    last_dial_attempt: Option<u64>,
    failure_count: u32,
    now: u64,
    dial_backoff_base: Duration,
    dial_backoff_max: Duration,
) -> bool {
    let Some(last_dial_attempt) = last_dial_attempt else {
        return false;
    };
    let backoff = dial_backoff_secs(
        failure_count,
        dial_backoff_base.as_secs(),
        dial_backoff_max.as_secs(),
    );
    now < last_dial_attempt.saturating_add(backoff)
}

fn entry_in_short_lived_exchange_backoff(
    entry: &ZakuraDiscoveryEntry,
    now: u64,
    dial_backoff_base: Duration,
    dial_backoff_max: Duration,
) -> bool {
    entry_metadata_in_short_lived_exchange_backoff(
        entry.last_short_lived_exchange,
        now,
        dial_backoff_base,
        dial_backoff_max,
    )
}

fn entry_metadata_in_short_lived_exchange_backoff(
    last_short_lived_exchange: Option<u64>,
    now: u64,
    dial_backoff_base: Duration,
    dial_backoff_max: Duration,
) -> bool {
    let Some(last_short_lived_exchange) = last_short_lived_exchange else {
        return false;
    };
    let backoff = dial_backoff_secs(1, dial_backoff_base.as_secs(), dial_backoff_max.as_secs());
    now < last_short_lived_exchange.saturating_add(backoff)
}

fn dial_backoff_secs(failure_count: u32, base_secs: u64, max_secs: u64) -> u64 {
    if failure_count == 0 {
        return 0;
    }

    let shift = failure_count.saturating_sub(1).min(10);
    base_secs.saturating_mul(1u64 << shift).min(max_secs)
}

fn node_id_sort_key(node_id: &NodeId) -> [u8; NODE_ID_BYTES] {
    *node_id.as_bytes()
}

fn validate_record_body_for_import(
    body: &ZakuraNodeRecordBody,
    context: &DiscoveryRecordValidationContext,
) -> Result<(), DiscoveryRecordError> {
    validate_record_body_bounds(body)?;
    if body.network_id != context.expected_network_id {
        return Err(DiscoveryRecordError::WrongNetwork);
    }
    if body.chain_id != context.expected_chain_id {
        return Err(DiscoveryRecordError::WrongChain);
    }
    if body.zakura_protocol_min > body.zakura_protocol_max
        || context.supported_protocol_min > context.supported_protocol_max
        || body.zakura_protocol_max < context.supported_protocol_min
        || context.supported_protocol_max < body.zakura_protocol_min
    {
        return Err(DiscoveryRecordError::IncompatibleProtocol);
    }

    let skew = context.clock_skew_tolerance.as_secs();
    if body.expires_at_unix_secs.saturating_add(skew) < context.current_unix_secs {
        return Err(DiscoveryRecordError::Expired);
    }

    let max_future = context
        .current_unix_secs
        .saturating_add(context.max_record_ttl.as_secs())
        .saturating_add(skew);
    if body.expires_at_unix_secs > max_future {
        return Err(DiscoveryRecordError::FarFutureExpiry);
    }

    Ok(())
}

fn validate_record_body_bounds(body: &ZakuraNodeRecordBody) -> Result<(), DiscoveryWireError> {
    if body.direct_addrs.len() > MAX_DIRECT_ADDRS_PER_RECORD {
        return Err(DiscoveryWireError::OversizedPayload {
            actual: body.direct_addrs.len(),
            max: MAX_DIRECT_ADDRS_PER_RECORD,
        });
    }
    if body.services.len() > MAX_SERVICES_PER_RECORD {
        return Err(DiscoveryWireError::OversizedPayload {
            actual: body.services.len(),
            max: MAX_SERVICES_PER_RECORD,
        });
    }
    for service in &body.services {
        validate_service_id(service.as_str().as_bytes())?;
    }
    if body.zakura_protocol_min > body.zakura_protocol_max {
        return Err(DiscoveryWireError::InvalidProtocolRange);
    }
    Ok(())
}

fn validate_query_fields(
    limit: u16,
    wanted_services: &[ZakuraServiceId],
    exclude_node_ids: &[NodeId],
) -> Result<(), DiscoveryWireError> {
    if usize::from(limit) > MAX_DISCOVERY_RECORDS_PER_RESPONSE {
        return Err(DiscoveryWireError::OversizedPayload {
            actual: usize::from(limit),
            max: MAX_DISCOVERY_RECORDS_PER_RESPONSE,
        });
    }
    if wanted_services.len() > MAX_SERVICES_PER_RECORD {
        return Err(DiscoveryWireError::OversizedPayload {
            actual: wanted_services.len(),
            max: MAX_SERVICES_PER_RECORD,
        });
    }
    if exclude_node_ids.len() > MAX_DISCOVERY_EXCLUDED_NODE_IDS {
        return Err(DiscoveryWireError::OversizedPayload {
            actual: exclude_node_ids.len(),
            max: MAX_DISCOVERY_EXCLUDED_NODE_IDS,
        });
    }
    for service in wanted_services {
        validate_service_id(service.as_str().as_bytes())?;
    }
    Ok(())
}

fn validate_get_services(query: &GetServices) -> Result<(), DiscoveryWireError> {
    validate_service_ids(&query.wanted_services, MAX_GET_SERVICES_WANTED)
}

fn validate_services(services: &Services) -> Result<(), DiscoveryWireError> {
    if services.summaries.len() > MAX_SERVICE_SUMMARIES_PER_RESPONSE {
        return Err(DiscoveryWireError::OversizedPayload {
            actual: services.summaries.len(),
            max: MAX_SERVICE_SUMMARIES_PER_RESPONSE,
        });
    }
    for summary in &services.summaries {
        validate_summary_envelope(summary)?;
    }
    Ok(())
}

fn validate_summary_envelope(envelope: &ServiceSummaryEnvelope) -> Result<(), DiscoveryWireError> {
    validate_service_id(envelope.service_id.as_str().as_bytes())?;
    if envelope.summary_bytes.len() > MAX_SERVICE_SUMMARY_BYTES {
        return Err(DiscoveryWireError::OversizedPayload {
            actual: envelope.summary_bytes.len(),
            max: MAX_SERVICE_SUMMARY_BYTES,
        });
    }
    validate_summary_service_binding(envelope.summary_tag, &envelope.service_id)?;
    validate_known_summary_payload(envelope.summary_tag, &envelope.summary_bytes)?;
    Ok(())
}

fn validate_summary_service_binding(
    summary_tag: u16,
    service_id: &ZakuraServiceId,
) -> Result<(), DiscoveryWireError> {
    let expected = match summary_tag {
        SUMMARY_TAG_HEADER_SYNC_V1 => Some(ZakuraServiceId::header_sync()),
        SUMMARY_TAG_DISCOVERY_V1 => Some(ZakuraServiceId::discovery()),
        SUMMARY_TAG_BLOCK_SYNC_V1 => Some(ZakuraServiceId::block_sync()),
        _ => None,
    };

    if expected
        .as_ref()
        .is_some_and(|expected| expected != service_id)
    {
        return Err(DiscoveryWireError::MismatchedSummaryService {
            summary_tag,
            service_id: service_id.clone(),
        });
    }

    Ok(())
}

fn validate_known_summary_payload(
    summary_tag: u16,
    summary_bytes: &[u8],
) -> Result<(), DiscoveryWireError> {
    match summary_tag {
        SUMMARY_TAG_HEADER_SYNC_V1 => {
            decode_header_sync_summary(summary_bytes)?;
        }
        SUMMARY_TAG_DISCOVERY_V1 => {
            decode_discovery_summary(summary_bytes)?;
        }
        SUMMARY_TAG_BLOCK_SYNC_V1 => {
            decode_block_sync_summary(summary_bytes)?;
        }
        _ => {}
    }
    Ok(())
}

fn validate_service_id(bytes: &[u8]) -> Result<(), DiscoveryWireError> {
    if bytes.is_empty() {
        return Err(DiscoveryWireError::Empty("service id"));
    }
    if bytes.len() > MAX_ZAKURA_SERVICE_ID_BYTES {
        return Err(DiscoveryWireError::OversizedPayload {
            actual: bytes.len(),
            max: MAX_ZAKURA_SERVICE_ID_BYTES,
        });
    }
    if !bytes.is_ascii() {
        return Err(DiscoveryWireError::NonAsciiServiceId);
    }
    Ok(())
}

fn validate_service_ids(
    services: &[ZakuraServiceId],
    max_count: usize,
) -> Result<(), DiscoveryWireError> {
    if services.len() > max_count {
        return Err(DiscoveryWireError::OversizedPayload {
            actual: services.len(),
            max: max_count,
        });
    }
    for service in services {
        validate_service_id(service.as_str().as_bytes())?;
    }
    Ok(())
}

fn encode_record(
    record: &ZakuraNodeRecord,
    writer: &mut impl Write,
) -> Result<(), DiscoveryWireError> {
    validate_record_body_bounds(&record.body)?;
    let mut body_bytes = Vec::new();
    encode_record_body_to(&record.body, &mut body_bytes)?;
    if body_bytes.len() > MAX_NODE_RECORD_BODY_BYTES {
        return Err(DiscoveryWireError::OversizedPayload {
            actual: body_bytes.len(),
            max: MAX_NODE_RECORD_BODY_BYTES,
        });
    }
    writer.write_u32::<LittleEndian>(u32_from_usize(body_bytes.len(), "record body length")?)?;
    writer.write_all(&body_bytes)?;
    writer.write_all(&record.signature.to_bytes())?;
    Ok(())
}

fn decode_record(reader: &mut impl Read) -> Result<ZakuraNodeRecord, DiscoveryWireError> {
    let body_len = usize_from_u32(reader.read_u32::<LittleEndian>()?, "record body length")?;
    if body_len > MAX_NODE_RECORD_BODY_BYTES {
        return Err(DiscoveryWireError::OversizedPayload {
            actual: body_len,
            max: MAX_NODE_RECORD_BODY_BYTES,
        });
    }

    let body_bytes = read_exact_vec(reader, body_len)?;
    let body = decode_record_body(&body_bytes)?;

    let mut signature_bytes = [0u8; SIGNATURE_BYTES];
    reader.read_exact(&mut signature_bytes)?;
    let signature = Signature::from(signature_bytes);

    Ok(ZakuraNodeRecord { body, signature })
}

fn encode_records_message(
    message_type: u8,
    records: &[ZakuraNodeRecord],
    max_count: usize,
    writer: &mut impl Write,
) -> Result<(), DiscoveryWireError> {
    if records.len() > max_count {
        return Err(DiscoveryWireError::OversizedPayload {
            actual: records.len(),
            max: max_count,
        });
    }
    writer.write_u8(message_type)?;
    writer.write_u16::<LittleEndian>(u16_from_usize(records.len(), "record count")?)?;
    for record in records {
        encode_record(record, writer)?;
    }
    Ok(())
}

fn decode_record_list(
    reader: &mut impl Read,
    max_count: usize,
) -> Result<Vec<ZakuraNodeRecord>, DiscoveryWireError> {
    let count = usize::from(reader.read_u16::<LittleEndian>()?);
    if count > max_count {
        return Err(DiscoveryWireError::OversizedPayload {
            actual: count,
            max: max_count,
        });
    }
    let mut records = Vec::with_capacity(count);
    for _ in 0..count {
        records.push(decode_record(reader)?);
    }
    Ok(records)
}

fn encode_query_fields(
    limit: u16,
    wanted_services: &[ZakuraServiceId],
    exclude_node_ids: &[NodeId],
    writer: &mut impl Write,
) -> Result<(), DiscoveryWireError> {
    writer.write_u16::<LittleEndian>(limit)?;
    encode_service_ids(wanted_services, writer)?;
    encode_node_ids(exclude_node_ids, writer)?;
    Ok(())
}

fn decode_query_fields(
    reader: &mut impl Read,
) -> Result<(u16, Vec<ZakuraServiceId>, Vec<NodeId>), DiscoveryWireError> {
    let limit = reader.read_u16::<LittleEndian>()?;
    let wanted_services = decode_service_ids(reader, MAX_SERVICES_PER_RECORD)?;
    let exclude_node_ids = decode_node_ids(reader, MAX_DISCOVERY_EXCLUDED_NODE_IDS)?;
    validate_query_fields(limit, &wanted_services, &exclude_node_ids)?;
    Ok((limit, wanted_services, exclude_node_ids))
}

fn decode_get_services(reader: &mut impl Read) -> Result<GetServices, DiscoveryWireError> {
    let wanted_services = decode_service_ids(reader, MAX_GET_SERVICES_WANTED)?;
    Ok(GetServices { wanted_services })
}

fn encode_services_message(
    services: &Services,
    writer: &mut impl Write,
) -> Result<(), DiscoveryWireError> {
    writer.write_all(services.node_id.as_bytes())?;
    writer.write_u64::<LittleEndian>(services.expires_at_unix_secs)?;
    writer.write_u16::<LittleEndian>(u16_from_usize(
        services.summaries.len(),
        "service summary count",
    )?)?;
    for summary in &services.summaries {
        encode_summary_envelope(summary, writer)?;
    }
    Ok(())
}

fn decode_services_message(reader: &mut Cursor<&[u8]>) -> Result<Services, DiscoveryWireError> {
    let node_id = decode_node_id(reader)?;
    let expires_at_unix_secs = reader.read_u64::<LittleEndian>()?;
    let count = usize::from(reader.read_u16::<LittleEndian>()?);
    if count > MAX_SERVICE_SUMMARIES_PER_RESPONSE {
        return Err(DiscoveryWireError::OversizedPayload {
            actual: count,
            max: MAX_SERVICE_SUMMARIES_PER_RESPONSE,
        });
    }

    let mut summaries = Vec::with_capacity(count);
    for _ in 0..count {
        summaries.push(decode_summary_envelope(reader)?);
    }

    let services = Services {
        node_id,
        expires_at_unix_secs,
        summaries,
    };
    validate_services(&services)?;
    Ok(services)
}

fn encode_summary_envelope(
    envelope: &ServiceSummaryEnvelope,
    writer: &mut impl Write,
) -> Result<(), DiscoveryWireError> {
    validate_summary_envelope(envelope)?;
    encode_service_id(&envelope.service_id, writer)?;
    writer.write_u16::<LittleEndian>(envelope.summary_tag)?;
    writer.write_u16::<LittleEndian>(u16_from_usize(
        envelope.summary_bytes.len(),
        "service summary length",
    )?)?;
    writer.write_all(&envelope.summary_bytes)?;
    Ok(())
}

fn decode_summary_envelope(
    reader: &mut Cursor<&[u8]>,
) -> Result<ServiceSummaryEnvelope, DiscoveryWireError> {
    let service_id = decode_service_id(reader)?;
    let summary_tag = reader.read_u16::<LittleEndian>()?;
    let summary_len = usize::from(reader.read_u16::<LittleEndian>()?);
    if summary_len > MAX_SERVICE_SUMMARY_BYTES {
        return Err(DiscoveryWireError::OversizedPayload {
            actual: summary_len,
            max: MAX_SERVICE_SUMMARY_BYTES,
        });
    }
    let remaining = cursor_remaining(reader)?;
    if summary_len > remaining {
        return Err(DiscoveryWireError::TruncatedPayload {
            declared: summary_len,
            remaining,
        });
    }
    let summary_bytes = read_exact_vec(reader, summary_len)?;
    validate_known_summary_payload(summary_tag, &summary_bytes)?;
    Ok(ServiceSummaryEnvelope {
        service_id,
        summary_tag,
        summary_bytes,
    })
}

fn encode_record_body_to(
    body: &ZakuraNodeRecordBody,
    writer: &mut impl Write,
) -> Result<(), DiscoveryWireError> {
    writer.write_all(body.node_id.as_bytes())?;
    encode_socket_addrs(&body.direct_addrs, writer)?;
    encode_service_ids(&body.services, writer)?;
    writer.write_u16::<LittleEndian>(body.zakura_protocol_min)?;
    writer.write_u16::<LittleEndian>(body.zakura_protocol_max)?;
    writer.write_u32::<LittleEndian>(body.network_id.code())?;
    writer.write_all(&body.chain_id)?;
    writer.write_u64::<LittleEndian>(body.sequence)?;
    writer.write_u64::<LittleEndian>(body.expires_at_unix_secs)?;
    Ok(())
}

fn decode_record_body(bytes: &[u8]) -> Result<ZakuraNodeRecordBody, DiscoveryWireError> {
    if bytes.len() > MAX_NODE_RECORD_BODY_BYTES {
        return Err(DiscoveryWireError::OversizedPayload {
            actual: bytes.len(),
            max: MAX_NODE_RECORD_BODY_BYTES,
        });
    }
    let mut reader = Cursor::new(bytes);
    let mut node_id_bytes = [0u8; NODE_ID_BYTES];
    reader.read_exact(&mut node_id_bytes)?;
    let node_id =
        NodeId::from_bytes(&node_id_bytes).map_err(|_| DiscoveryWireError::InvalidNodeId)?;
    let direct_addrs = decode_socket_addrs(&mut reader)?;
    let services = decode_service_ids(&mut reader, MAX_SERVICES_PER_RECORD)?;
    let zakura_protocol_min = reader.read_u16::<LittleEndian>()?;
    let zakura_protocol_max = reader.read_u16::<LittleEndian>()?;
    let network_id = network_id_from_code(reader.read_u32::<LittleEndian>()?)?;
    let mut chain_id = [0u8; 32];
    reader.read_exact(&mut chain_id)?;
    let sequence = reader.read_u64::<LittleEndian>()?;
    let expires_at_unix_secs = reader.read_u64::<LittleEndian>()?;
    reject_trailing(bytes, &reader)?;

    let body = ZakuraNodeRecordBody {
        node_id,
        direct_addrs,
        services,
        zakura_protocol_min,
        zakura_protocol_max,
        network_id,
        chain_id,
        sequence,
        expires_at_unix_secs,
    };
    validate_record_body_bounds(&body)?;
    Ok(body)
}

fn encode_socket_addrs(
    addrs: &[SocketAddr],
    writer: &mut impl Write,
) -> Result<(), DiscoveryWireError> {
    if addrs.len() > MAX_DIRECT_ADDRS_PER_RECORD {
        return Err(DiscoveryWireError::OversizedPayload {
            actual: addrs.len(),
            max: MAX_DIRECT_ADDRS_PER_RECORD,
        });
    }
    writer.write_u16::<LittleEndian>(u16_from_usize(addrs.len(), "direct address count")?)?;
    for addr in addrs {
        match addr {
            SocketAddr::V4(addr) => {
                writer.write_u8(SOCKET_ADDR_V4)?;
                writer.write_all(&addr.ip().octets())?;
                writer.write_u16::<LittleEndian>(addr.port())?;
            }
            SocketAddr::V6(addr) => {
                writer.write_u8(SOCKET_ADDR_V6)?;
                writer.write_all(&addr.ip().octets())?;
                writer.write_u16::<LittleEndian>(addr.port())?;
                writer.write_u32::<LittleEndian>(addr.flowinfo())?;
                writer.write_u32::<LittleEndian>(addr.scope_id())?;
            }
        }
    }
    Ok(())
}

fn decode_socket_addrs(reader: &mut impl Read) -> Result<Vec<SocketAddr>, DiscoveryWireError> {
    let count = usize::from(reader.read_u16::<LittleEndian>()?);
    if count > MAX_DIRECT_ADDRS_PER_RECORD {
        return Err(DiscoveryWireError::OversizedPayload {
            actual: count,
            max: MAX_DIRECT_ADDRS_PER_RECORD,
        });
    }
    let mut addrs = Vec::with_capacity(count);
    for _ in 0..count {
        let addr = match reader.read_u8()? {
            SOCKET_ADDR_V4 => {
                let mut octets = [0u8; 4];
                reader.read_exact(&mut octets)?;
                let port = reader.read_u16::<LittleEndian>()?;
                SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::from(octets), port))
            }
            SOCKET_ADDR_V6 => {
                let mut octets = [0u8; 16];
                reader.read_exact(&mut octets)?;
                let port = reader.read_u16::<LittleEndian>()?;
                let flowinfo = reader.read_u32::<LittleEndian>()?;
                let scope_id = reader.read_u32::<LittleEndian>()?;
                SocketAddr::V6(SocketAddrV6::new(
                    Ipv6Addr::from(octets),
                    port,
                    flowinfo,
                    scope_id,
                ))
            }
            family => return Err(DiscoveryWireError::InvalidAddressFamily(family)),
        };
        addrs.push(addr);
    }
    Ok(addrs)
}

fn encode_service_ids(
    services: &[ZakuraServiceId],
    writer: &mut impl Write,
) -> Result<(), DiscoveryWireError> {
    encode_service_ids_capped(services, MAX_SERVICES_PER_RECORD, writer)
}

fn encode_service_ids_capped(
    services: &[ZakuraServiceId],
    max_count: usize,
    writer: &mut impl Write,
) -> Result<(), DiscoveryWireError> {
    if services.len() > max_count {
        return Err(DiscoveryWireError::OversizedPayload {
            actual: services.len(),
            max: max_count,
        });
    }
    writer.write_u16::<LittleEndian>(u16_from_usize(services.len(), "service count")?)?;
    for service in services {
        encode_service_id(service, writer)?;
    }
    Ok(())
}

fn encode_service_id(
    service: &ZakuraServiceId,
    writer: &mut impl Write,
) -> Result<(), DiscoveryWireError> {
    let bytes = service.as_str().as_bytes();
    validate_service_id(bytes)?;
    writer.write_u16::<LittleEndian>(u16_from_usize(bytes.len(), "service id length")?)?;
    writer.write_all(bytes)?;
    Ok(())
}

fn decode_service_ids(
    reader: &mut impl Read,
    max_count: usize,
) -> Result<Vec<ZakuraServiceId>, DiscoveryWireError> {
    let count = usize::from(reader.read_u16::<LittleEndian>()?);
    if count > max_count {
        return Err(DiscoveryWireError::OversizedPayload {
            actual: count,
            max: max_count,
        });
    }
    let mut services = Vec::with_capacity(count);
    for _ in 0..count {
        services.push(decode_service_id(reader)?);
    }
    Ok(services)
}

fn decode_service_id(reader: &mut impl Read) -> Result<ZakuraServiceId, DiscoveryWireError> {
    let len = usize::from(reader.read_u16::<LittleEndian>()?);
    if len > MAX_ZAKURA_SERVICE_ID_BYTES {
        return Err(DiscoveryWireError::OversizedPayload {
            actual: len,
            max: MAX_ZAKURA_SERVICE_ID_BYTES,
        });
    }
    let bytes = read_exact_vec(reader, len)?;
    validate_service_id(&bytes)?;
    let service = String::from_utf8(bytes).map_err(|_| DiscoveryWireError::NonAsciiServiceId)?;
    Ok(ZakuraServiceId(service))
}

fn encode_node_ids(node_ids: &[NodeId], writer: &mut impl Write) -> Result<(), DiscoveryWireError> {
    if node_ids.len() > MAX_DISCOVERY_EXCLUDED_NODE_IDS {
        return Err(DiscoveryWireError::OversizedPayload {
            actual: node_ids.len(),
            max: MAX_DISCOVERY_EXCLUDED_NODE_IDS,
        });
    }
    writer.write_u16::<LittleEndian>(u16_from_usize(node_ids.len(), "node id count")?)?;
    for node_id in node_ids {
        writer.write_all(node_id.as_bytes())?;
    }
    Ok(())
}

fn decode_node_ids(
    reader: &mut impl Read,
    max_count: usize,
) -> Result<Vec<NodeId>, DiscoveryWireError> {
    let count = usize::from(reader.read_u16::<LittleEndian>()?);
    if count > max_count {
        return Err(DiscoveryWireError::OversizedPayload {
            actual: count,
            max: max_count,
        });
    }
    let mut node_ids = Vec::with_capacity(count);
    for _ in 0..count {
        node_ids.push(decode_node_id(reader)?);
    }
    Ok(node_ids)
}

fn decode_node_id(reader: &mut impl Read) -> Result<NodeId, DiscoveryWireError> {
    let mut bytes = [0u8; NODE_ID_BYTES];
    reader.read_exact(&mut bytes)?;
    NodeId::from_bytes(&bytes).map_err(|_| DiscoveryWireError::InvalidNodeId)
}

fn network_id_from_code(value: u32) -> Result<ZakuraNetworkId, DiscoveryWireError> {
    match value {
        1 => Ok(ZakuraNetworkId::Mainnet),
        2 => Ok(ZakuraNetworkId::Testnet),
        3 => Ok(ZakuraNetworkId::Regtest),
        4 => Ok(ZakuraNetworkId::Configured),
        _ => Err(DiscoveryWireError::InvalidNetworkId(value)),
    }
}

fn encode_header_sync_summary(
    summary: &HeaderSyncServiceSummary,
    writer: &mut impl Write,
) -> Result<(), DiscoveryWireError> {
    write_height(writer, summary.best_height)?;
    writer.write_all(&summary.best_hash.0)?;
    write_optional_height(writer, summary.finalized_height)?;
    write_bool(writer, summary.serving_headers)?;
    writer.write_u16::<LittleEndian>(summary.inbound_slots_free)?;
    writer.write_u16::<LittleEndian>(summary.inbound_slots_max)?;
    writer.write_u16::<LittleEndian>(summary.outbound_slots_free)?;
    writer.write_u16::<LittleEndian>(summary.outbound_slots_max)?;
    Ok(())
}

fn decode_header_sync_summary(
    bytes: &[u8],
) -> Result<HeaderSyncServiceSummary, DiscoveryWireError> {
    if bytes.len() > MAX_SERVICE_SUMMARY_BYTES {
        return Err(DiscoveryWireError::OversizedPayload {
            actual: bytes.len(),
            max: MAX_SERVICE_SUMMARY_BYTES,
        });
    }
    let mut reader = Cursor::new(bytes);
    let best_height = read_height(&mut reader)?;
    let mut best_hash = [0u8; 32];
    reader.read_exact(&mut best_hash)?;
    let finalized_height = read_optional_height(&mut reader)?;
    let serving_headers = read_bool(&mut reader, "header sync serving_headers")?;
    let inbound_slots_free = reader.read_u16::<LittleEndian>()?;
    let inbound_slots_max = reader.read_u16::<LittleEndian>()?;
    let outbound_slots_free = reader.read_u16::<LittleEndian>()?;
    let outbound_slots_max = reader.read_u16::<LittleEndian>()?;
    reject_trailing(bytes, &reader)?;
    Ok(HeaderSyncServiceSummary {
        best_height,
        best_hash: block::Hash(best_hash),
        finalized_height,
        serving_headers,
        inbound_slots_free,
        inbound_slots_max,
        outbound_slots_free,
        outbound_slots_max,
    })
}

fn encode_discovery_summary(
    summary: &DiscoveryServiceSummary,
    writer: &mut impl Write,
) -> Result<(), DiscoveryWireError> {
    writer.write_u16::<LittleEndian>(summary.peer_exchange_slots_free)?;
    writer.write_u16::<LittleEndian>(summary.max_records_per_response)?;
    write_bool(writer, summary.expected_disconnect_after_exchange)?;
    Ok(())
}

fn decode_discovery_summary(bytes: &[u8]) -> Result<DiscoveryServiceSummary, DiscoveryWireError> {
    if bytes.len() > MAX_SERVICE_SUMMARY_BYTES {
        return Err(DiscoveryWireError::OversizedPayload {
            actual: bytes.len(),
            max: MAX_SERVICE_SUMMARY_BYTES,
        });
    }
    let mut reader = Cursor::new(bytes);
    let peer_exchange_slots_free = reader.read_u16::<LittleEndian>()?;
    let max_records_per_response = reader.read_u16::<LittleEndian>()?;
    let expected_disconnect_after_exchange =
        read_bool(&mut reader, "discovery expected_disconnect_after_exchange")?;
    reject_trailing(bytes, &reader)?;
    Ok(DiscoveryServiceSummary {
        peer_exchange_slots_free,
        max_records_per_response,
        expected_disconnect_after_exchange,
    })
}

fn encode_block_sync_summary(
    summary: &BlockSyncServiceSummary,
    writer: &mut impl Write,
) -> Result<(), DiscoveryWireError> {
    validate_block_sync_summary(summary)?;
    write_height(writer, summary.servable_low)?;
    write_height(writer, summary.servable_high)?;
    writer.write_all(&summary.tip_hash.0)?;
    writer.write_u16::<LittleEndian>(summary.free_slots)?;
    writer.write_u32::<LittleEndian>(summary.max_blocks_per_response)?;
    writer.write_u32::<LittleEndian>(summary.max_response_bytes)?;
    Ok(())
}

fn decode_block_sync_summary(bytes: &[u8]) -> Result<BlockSyncServiceSummary, DiscoveryWireError> {
    if bytes.len() > MAX_SERVICE_SUMMARY_BYTES {
        return Err(DiscoveryWireError::OversizedPayload {
            actual: bytes.len(),
            max: MAX_SERVICE_SUMMARY_BYTES,
        });
    }
    let mut reader = Cursor::new(bytes);
    let servable_low = read_height(&mut reader)?;
    let servable_high = read_height(&mut reader)?;
    let mut tip_hash = [0u8; 32];
    reader.read_exact(&mut tip_hash)?;
    let free_slots = reader.read_u16::<LittleEndian>()?;
    let max_blocks_per_response = reader.read_u32::<LittleEndian>()?;
    let max_response_bytes = reader.read_u32::<LittleEndian>()?;
    reject_trailing(bytes, &reader)?;
    let summary = BlockSyncServiceSummary {
        servable_low,
        servable_high,
        tip_hash: block::Hash(tip_hash),
        free_slots,
        max_blocks_per_response,
        max_response_bytes,
    };
    validate_block_sync_summary(&summary)?;
    Ok(summary)
}

fn validate_block_sync_summary(
    summary: &BlockSyncServiceSummary,
) -> Result<(), DiscoveryWireError> {
    if summary.servable_low > summary.servable_high {
        return Err(DiscoveryWireError::InvalidProtocolRange);
    }
    if summary.max_blocks_per_response == 0
        || summary.max_blocks_per_response > MAX_BS_BLOCKS_PER_REQUEST
    {
        return Err(DiscoveryWireError::OversizedPayload {
            actual: usize::try_from(summary.max_blocks_per_response).unwrap_or(usize::MAX),
            max: usize::try_from(MAX_BS_BLOCKS_PER_REQUEST).unwrap_or(usize::MAX),
        });
    }
    if summary.max_response_bytes == 0 || summary.max_response_bytes > MAX_BS_RESPONSE_BYTES {
        return Err(DiscoveryWireError::OversizedPayload {
            actual: usize::try_from(summary.max_response_bytes).unwrap_or(usize::MAX),
            max: usize::try_from(MAX_BS_RESPONSE_BYTES).unwrap_or(usize::MAX),
        });
    }
    Ok(())
}

fn write_height(writer: &mut impl Write, height: block::Height) -> Result<(), DiscoveryWireError> {
    writer.write_u32::<LittleEndian>(height.0)?;
    Ok(())
}

fn read_height(reader: &mut impl Read) -> Result<block::Height, DiscoveryWireError> {
    let height = reader.read_u32::<LittleEndian>()?;
    block::Height::try_from(height).map_err(|_| DiscoveryWireError::InvalidHeight(height))
}

fn write_optional_height(
    writer: &mut impl Write,
    height: Option<block::Height>,
) -> Result<(), DiscoveryWireError> {
    match height {
        Some(height) => {
            writer.write_u8(1)?;
            write_height(writer, height)?;
        }
        None => writer.write_u8(0)?,
    }
    Ok(())
}

fn read_optional_height(
    reader: &mut impl Read,
) -> Result<Option<block::Height>, DiscoveryWireError> {
    match reader.read_u8()? {
        0 => Ok(None),
        1 => read_height(reader).map(Some),
        value => Err(DiscoveryWireError::InvalidBoolean {
            field: "optional height present",
            value,
        }),
    }
}

fn write_bool(writer: &mut impl Write, value: bool) -> Result<(), DiscoveryWireError> {
    writer.write_u8(u8::from(value))?;
    Ok(())
}

fn read_bool(reader: &mut impl Read, field: &'static str) -> Result<bool, DiscoveryWireError> {
    match reader.read_u8()? {
        0 => Ok(false),
        1 => Ok(true),
        value => Err(DiscoveryWireError::InvalidBoolean { field, value }),
    }
}

fn saturating_u16(value: usize) -> u16 {
    u16::try_from(value).unwrap_or(u16::MAX)
}

fn cursor_remaining(reader: &Cursor<&[u8]>) -> Result<usize, DiscoveryWireError> {
    let position = usize::try_from(reader.position())
        .map_err(|_| DiscoveryWireError::NumericOverflow("cursor position"))?;
    Ok(reader.get_ref().len().saturating_sub(position))
}

fn read_exact_vec(reader: &mut impl Read, len: usize) -> Result<Vec<u8>, DiscoveryWireError> {
    let mut bytes = vec![0; len];
    reader.read_exact(&mut bytes)?;
    Ok(bytes)
}

fn reject_trailing(bytes: &[u8], reader: &Cursor<&[u8]>) -> Result<(), DiscoveryWireError> {
    let consumed = usize::try_from(reader.position())
        .map_err(|_| DiscoveryWireError::NumericOverflow("cursor position"))?;
    if consumed != bytes.len() {
        return Err(DiscoveryWireError::TrailingBytes);
    }
    Ok(())
}

fn usize_from_u32(value: u32, field: &'static str) -> Result<usize, DiscoveryWireError> {
    usize::try_from(value).map_err(|_| DiscoveryWireError::NumericOverflow(field))
}

fn u32_from_usize(value: usize, field: &'static str) -> Result<u32, DiscoveryWireError> {
    u32::try_from(value).map_err(|_| DiscoveryWireError::NumericOverflow(field))
}

fn u16_from_usize(value: usize, field: &'static str) -> Result<u16, DiscoveryWireError> {
    u16::try_from(value).map_err(|_| DiscoveryWireError::NumericOverflow(field))
}

#[cfg(test)]
mod tests {
    use std::{net::IpAddr, time::Duration};

    use iroh::SecretKey;
    use rand::{rngs::OsRng, rngs::StdRng, SeedableRng};

    use super::*;

    const NOW: u64 = 1_700_000_000;
    const CHAIN_ID: [u8; 32] = [7; 32];

    fn secret_key() -> SecretKey {
        SecretKey::generate(OsRng)
    }

    fn service(index: usize) -> ZakuraServiceId {
        ZakuraServiceId::new(format!("zakura.test.{index}.v1")).expect("test service id is valid")
    }

    fn context() -> DiscoveryRecordValidationContext {
        DiscoveryRecordValidationContext {
            expected_network_id: ZakuraNetworkId::Regtest,
            expected_chain_id: CHAIN_ID,
            current_unix_secs: NOW,
            supported_protocol_min: 1,
            supported_protocol_max: 1,
            max_record_ttl: Duration::from_secs(24 * 60 * 60),
            clock_skew_tolerance: Duration::from_secs(300),
        }
    }

    fn body(secret_key: &SecretKey) -> ZakuraNodeRecordBody {
        ZakuraNodeRecordBody {
            node_id: secret_key.public(),
            direct_addrs: vec![
                SocketAddr::new(IpAddr::V4(Ipv4Addr::new(192, 0, 2, 10)), 8233),
                SocketAddr::new(
                    IpAddr::V6(Ipv6Addr::new(0x2001, 0x0db8, 0, 0, 0, 0, 0, 10)),
                    18233,
                ),
            ],
            services: vec![
                ZakuraServiceId::discovery(),
                ZakuraServiceId::legacy_gossip(),
                ZakuraServiceId::legacy_requests(),
            ],
            zakura_protocol_min: 1,
            zakura_protocol_max: 1,
            network_id: ZakuraNetworkId::Regtest,
            chain_id: CHAIN_ID,
            sequence: 42,
            expires_at_unix_secs: NOW + 60,
        }
    }

    fn signed_record() -> ZakuraNodeRecord {
        let secret_key = secret_key();
        ZakuraNodeRecord::sign(body(&secret_key), &secret_key).expect("test record signs")
    }

    fn sign_record_with_prefix(
        body: ZakuraNodeRecordBody,
        secret_key: &SecretKey,
        domain: &[u8],
        record_format_version: u16,
    ) -> ZakuraNodeRecord {
        let mut bytes = Vec::new();
        bytes.write_all(domain).expect("test domain writes");
        bytes
            .write_u16::<LittleEndian>(record_format_version)
            .expect("test record format version writes");
        encode_record_body_to(&body, &mut bytes).expect("test record body encodes");

        let signing_key = SigningKey::from(secret_key.to_bytes());
        let signature = signing_key.sign(&bytes);
        ZakuraNodeRecord { body, signature }
    }

    fn signed_record_with(
        sequence: u64,
        service: ZakuraServiceId,
        addr: SocketAddr,
    ) -> ZakuraNodeRecord {
        signed_record_with_addrs(sequence, service, vec![addr])
    }

    fn signed_record_with_addrs(
        sequence: u64,
        service: ZakuraServiceId,
        addrs: Vec<SocketAddr>,
    ) -> ZakuraNodeRecord {
        let secret_key = secret_key();
        let mut body = body(&secret_key);
        body.sequence = sequence;
        body.direct_addrs = addrs;
        body.services = vec![service];
        ZakuraNodeRecord::sign(body, &secret_key).expect("test record signs")
    }

    fn record_with_secret(
        secret_key: &SecretKey,
        sequence: u64,
        service: ZakuraServiceId,
    ) -> ZakuraNodeRecord {
        let mut body = body(secret_key);
        body.sequence = sequence;
        body.services = vec![service];
        ZakuraNodeRecord::sign(body, secret_key).expect("test record signs")
    }

    fn test_addr(index: u8) -> SocketAddr {
        SocketAddr::new(IpAddr::V4(Ipv4Addr::new(192, 0, 2, index)), 8233)
    }

    fn candidate_for(record: &ZakuraNodeRecord, is_static: bool) -> ZakuraDiscoveryDialCandidate {
        ZakuraDiscoveryDialCandidate {
            node_id: record.body.node_id,
            direct_addrs: record.body.direct_addrs.clone(),
            is_static,
        }
    }

    fn import_confirmed_record(
        book: &mut ZakuraDiscoveryBook,
        record: ZakuraNodeRecord,
    ) -> Result<ImportOutcome, DiscoveryBookError> {
        let source = record.body.node_id;
        book.import_record(record, Some(source), NOW, &context())
    }

    fn small_book(max_records: usize) -> ZakuraDiscoveryBook {
        ZakuraDiscoveryBook::new(ZakuraDiscoveryBookLimits {
            max_records,
            ..ZakuraDiscoveryBookLimits::default()
        })
    }

    fn runtime_record_with(
        sequence: u64,
        service: ZakuraServiceId,
        addr: SocketAddr,
    ) -> ZakuraNodeRecord {
        let secret_key = secret_key();
        runtime_record_with_secret(&secret_key, sequence, service, addr)
    }

    fn runtime_record_with_secret(
        secret_key: &SecretKey,
        sequence: u64,
        service: ZakuraServiceId,
        addr: SocketAddr,
    ) -> ZakuraNodeRecord {
        runtime_record_with_secret_and_addrs(secret_key, sequence, service, vec![addr])
    }

    fn runtime_record_with_secret_and_addrs(
        secret_key: &SecretKey,
        sequence: u64,
        service: ZakuraServiceId,
        addrs: Vec<SocketAddr>,
    ) -> ZakuraNodeRecord {
        let mut body = body(secret_key);
        body.sequence = sequence;
        body.direct_addrs = addrs;
        body.services = vec![service];
        body.expires_at_unix_secs = current_unix_secs() + DEFAULT_DISCOVERY_RECORD_TTL.as_secs();
        ZakuraNodeRecord::sign(body, secret_key).expect("runtime test record signs")
    }

    fn local_config_with(
        secret_key: SecretKey,
        direct_addrs: Vec<SocketAddr>,
        services: Vec<ZakuraServiceId>,
    ) -> ZakuraDiscoveryLocalConfig {
        ZakuraDiscoveryLocalConfig {
            secret_key,
            direct_addrs,
            services,
            zakura_protocol_min: 1,
            zakura_protocol_max: 1,
            network_id: ZakuraNetworkId::Regtest,
            chain_id: CHAIN_ID,
            last_authored_sequence: None,
        }
    }

    fn discovery_handle_at(
        local_config: ZakuraDiscoveryLocalConfig,
        config: ZakuraDiscoveryConfig,
        connected: watch::Receiver<Vec<ZakuraPeerId>>,
        now: u64,
    ) -> ZakuraDiscoveryHandle {
        ZakuraDiscoveryHandle::new_at(local_config, config, connected, now, now)
            .expect("test discovery state constructs")
    }

    fn discovery_handle_at_with_sequence(
        local_config: ZakuraDiscoveryLocalConfig,
        config: ZakuraDiscoveryConfig,
        connected: watch::Receiver<Vec<ZakuraPeerId>>,
        now: u64,
        wall_clock_sequence: u64,
    ) -> ZakuraDiscoveryHandle {
        ZakuraDiscoveryHandle::new_at(local_config, config, connected, now, wall_clock_sequence)
            .expect("test discovery state constructs")
    }

    fn discovery_handle_with_connected(
        connected: watch::Receiver<Vec<ZakuraPeerId>>,
    ) -> ZakuraDiscoveryHandle {
        discovery_handle_at(
            local_config_with(
                secret_key(),
                vec![test_addr(200)],
                vec![ZakuraServiceId::discovery()],
            ),
            ZakuraDiscoveryConfig::default(),
            connected,
            NOW,
        )
    }

    fn peer_id_for(node_id: NodeId) -> ZakuraPeerId {
        ZakuraPeerId::new(node_id.as_bytes().to_vec()).expect("node id is a valid peer id")
    }

    fn header_summary() -> HeaderSyncServiceSummary {
        HeaderSyncServiceSummary {
            best_height: block::Height(42),
            best_hash: block::Hash([7; 32]),
            finalized_height: Some(block::Height(40)),
            serving_headers: true,
            inbound_slots_free: 3,
            inbound_slots_max: 10,
            outbound_slots_free: 4,
            outbound_slots_max: 11,
        }
    }

    fn discovery_summary() -> DiscoveryServiceSummary {
        DiscoveryServiceSummary {
            peer_exchange_slots_free: 5,
            max_records_per_response: 12,
            expected_disconnect_after_exchange: true,
        }
    }

    fn block_sync_summary() -> BlockSyncServiceSummary {
        BlockSyncServiceSummary {
            servable_low: block::Height(10),
            servable_high: block::Height(42),
            tip_hash: block::Hash([9; 32]),
            free_slots: 3,
            max_blocks_per_response: 16,
            max_response_bytes: MAX_BS_RESPONSE_BYTES,
        }
    }

    fn block_sync_status() -> BlockSyncStatus {
        BlockSyncStatus {
            servable_low: block::Height(10),
            servable_high: block::Height(42),
            tip_hash: block::Hash([9; 32]),
            max_blocks_per_response: 16,
            max_inflight_requests: 4,
            max_response_bytes: MAX_BS_RESPONSE_BYTES,
        }
    }

    fn services_message(summaries: Vec<ServiceSummaryEnvelope>) -> DiscoveryMessage {
        DiscoveryMessage::Services(Services {
            node_id: secret_key().public(),
            expires_at_unix_secs: NOW + 30,
            summaries,
        })
    }

    fn first_party_services(
        node_id: NodeId,
        expires_at_unix_secs: u64,
        summaries: Vec<ServiceSummaryEnvelope>,
    ) -> Services {
        Services {
            node_id,
            expires_at_unix_secs,
            summaries,
        }
    }

    #[test]
    fn service_id_accepts_valid_ids_and_rejects_invalid_ids() {
        assert_eq!(
            ZakuraServiceId::new("zakura.custom.v1").unwrap().as_str(),
            "zakura.custom.v1"
        );
        assert!(matches!(
            ZakuraServiceId::new(""),
            Err(DiscoveryWireError::Empty("service id"))
        ));
        assert!(matches!(
            ZakuraServiceId::new("zakura.\u{2603}.v1"),
            Err(DiscoveryWireError::NonAsciiServiceId)
        ));
        assert!(matches!(
            ZakuraServiceId::new("a".repeat(MAX_ZAKURA_SERVICE_ID_BYTES + 1)),
            Err(DiscoveryWireError::OversizedPayload { .. })
        ));
    }

    #[test]
    fn block_sync_summary_uses_live_peer_slots() {
        let limits = ServicePeerLimits {
            max_inbound_peers: 2,
            ..ServicePeerLimits::default()
        };
        let status = block_sync_status();

        let open = BlockSyncServiceSummary::from_status_and_snapshot(
            status,
            ServicePeerSnapshot::new(1, 0, limits),
        );
        assert_eq!(open.free_slots, 1);

        let full = BlockSyncServiceSummary::from_status_and_snapshot(
            status,
            ServicePeerSnapshot::new(2, 0, limits),
        );
        assert_eq!(full.free_slots, 0);
    }

    #[test]
    fn node_record_encode_decode_roundtrip() {
        let record = signed_record();
        let signed_bytes = record.body.encode_for_signature().expect("record signs");
        let mut signed_bytes_reader =
            Cursor::new(&signed_bytes[ZAKURA_NODE_RECORD_SIG_DOMAIN.len()..]);
        assert!(signed_bytes.starts_with(ZAKURA_NODE_RECORD_SIG_DOMAIN));
        assert_eq!(
            signed_bytes_reader.read_u16::<LittleEndian>().unwrap(),
            ZAKURA_NODE_RECORD_FORMAT_VERSION
        );

        let mut body_bytes = Vec::new();
        encode_record_body_to(&record.body, &mut body_bytes).expect("record body encodes");
        assert_eq!(
            &signed_bytes[ZAKURA_NODE_RECORD_SIG_DOMAIN.len() + 2..],
            body_bytes.as_slice()
        );

        let mut bytes = Vec::new();
        encode_record(&record, &mut bytes).expect("record encodes");
        let encoded_body_len =
            usize::try_from(Cursor::new(&bytes).read_u32::<LittleEndian>().unwrap())
                .expect("encoded record body length fits in usize");
        assert_eq!(encoded_body_len, body_bytes.len());

        let decoded = decode_record(&mut Cursor::new(&bytes)).expect("record decodes");

        assert_eq!(decoded, record);
        assert_eq!(decoded.body.encode_for_signature().unwrap(), signed_bytes);
        decoded.verify(&context()).expect("record verifies");
    }

    #[test]
    fn discovery_message_roundtrips_every_variant() {
        let record = signed_record();
        let other = signed_record();
        let services = vec![ZakuraServiceId::discovery(), service(1)];
        let excluded = vec![record.body.node_id, other.body.node_id];
        let header_envelope =
            ServiceSummaryEnvelope::header_sync(&header_summary()).expect("summary encodes");
        let discovery_envelope =
            ServiceSummaryEnvelope::discovery(&discovery_summary()).expect("summary encodes");
        let block_envelope =
            ServiceSummaryEnvelope::block_sync(&block_sync_summary()).expect("summary encodes");
        let messages = vec![
            DiscoveryMessage::Hello {
                record: record.clone(),
            },
            DiscoveryMessage::GetPeers {
                limit: 2,
                wanted_services: services.clone(),
                exclude_node_ids: excluded.clone(),
            },
            DiscoveryMessage::Peers {
                records: vec![record.clone(), other.clone()],
            },
            DiscoveryMessage::GetServices(GetServices {
                wanted_services: services,
            }),
            services_message(vec![header_envelope, discovery_envelope, block_envelope]),
        ];

        for message in messages {
            let encoded = message.encode().expect("message encodes");
            assert_eq!(DiscoveryMessage::decode(&encoded).unwrap(), message);
        }
    }

    #[test]
    fn service_summary_envelopes_roundtrip_and_unknown_tags_are_skipped() {
        let header = header_summary();
        let discovery = discovery_summary();
        let block = block_sync_summary();
        let header_envelope =
            ServiceSummaryEnvelope::header_sync(&header).expect("header summary encodes");
        let discovery_envelope =
            ServiceSummaryEnvelope::discovery(&discovery).expect("discovery summary encodes");
        let block_envelope =
            ServiceSummaryEnvelope::block_sync(&block).expect("block summary encodes");
        let unknown_envelope = ServiceSummaryEnvelope {
            service_id: service(99),
            summary_tag: 999,
            summary_bytes: vec![1, 2, 3, 4],
        };

        assert_eq!(header_envelope.decode_header_sync().unwrap(), Some(header));
        assert_eq!(
            discovery_envelope.decode_discovery().unwrap(),
            Some(discovery)
        );
        assert_eq!(block_envelope.decode_block_sync().unwrap(), Some(block));

        let message = services_message(vec![
            unknown_envelope.clone(),
            header_envelope.clone(),
            discovery_envelope.clone(),
            block_envelope.clone(),
        ]);
        let decoded = DiscoveryMessage::decode(&message.encode().expect("message encodes"))
            .expect("message decodes");

        let DiscoveryMessage::Services(services) = decoded else {
            panic!("expected service response");
        };
        assert_eq!(services.summaries[0], unknown_envelope);
        assert_eq!(
            services.summaries[1].decode_header_sync().unwrap(),
            Some(header)
        );
        assert_eq!(
            services.summaries[2].decode_discovery().unwrap(),
            Some(discovery)
        );
        assert_eq!(
            services.summaries[3].decode_block_sync().unwrap(),
            Some(block)
        );
    }

    #[test]
    fn service_summary_decode_rejects_lying_truncated_and_oversized_lengths() {
        let node_id = secret_key().public();
        let mut lying = vec![MSG_DISCOVERY_SERVICES];
        lying.write_all(node_id.as_bytes()).unwrap();
        lying.write_u64::<LittleEndian>(NOW + 30).unwrap();
        lying.write_u16::<LittleEndian>(1).unwrap();
        encode_service_id(&service(1), &mut lying).unwrap();
        lying.write_u16::<LittleEndian>(999).unwrap();
        lying.write_u16::<LittleEndian>(4).unwrap();
        lying.write_u8(1).unwrap();

        assert!(matches!(
            DiscoveryMessage::decode(&lying),
            Err(DiscoveryWireError::TruncatedPayload { .. })
        ));

        let mut truncated_known = vec![MSG_DISCOVERY_SERVICES];
        truncated_known.write_all(node_id.as_bytes()).unwrap();
        truncated_known.write_u64::<LittleEndian>(NOW + 30).unwrap();
        truncated_known.write_u16::<LittleEndian>(1).unwrap();
        encode_service_id(&ZakuraServiceId::header_sync(), &mut truncated_known).unwrap();
        truncated_known
            .write_u16::<LittleEndian>(SUMMARY_TAG_HEADER_SYNC_V1)
            .unwrap();
        truncated_known.write_u16::<LittleEndian>(1).unwrap();
        truncated_known.write_u8(0).unwrap();

        assert!(DiscoveryMessage::decode(&truncated_known).is_err());

        let mut truncated_block = vec![MSG_DISCOVERY_SERVICES];
        truncated_block.write_all(node_id.as_bytes()).unwrap();
        truncated_block.write_u64::<LittleEndian>(NOW + 30).unwrap();
        truncated_block.write_u16::<LittleEndian>(1).unwrap();
        encode_service_id(&ZakuraServiceId::block_sync(), &mut truncated_block).unwrap();
        truncated_block
            .write_u16::<LittleEndian>(SUMMARY_TAG_BLOCK_SYNC_V1)
            .unwrap();
        truncated_block.write_u16::<LittleEndian>(8).unwrap();
        write_height(&mut truncated_block, block::Height(10)).unwrap();
        write_height(&mut truncated_block, block::Height(42)).unwrap();

        assert!(DiscoveryMessage::decode(&truncated_block).is_err());

        let mut oversized = vec![MSG_DISCOVERY_SERVICES];
        oversized.write_all(node_id.as_bytes()).unwrap();
        oversized.write_u64::<LittleEndian>(NOW + 30).unwrap();
        oversized.write_u16::<LittleEndian>(1).unwrap();
        encode_service_id(&ZakuraServiceId::block_sync(), &mut oversized).unwrap();
        oversized
            .write_u16::<LittleEndian>(SUMMARY_TAG_BLOCK_SYNC_V1)
            .unwrap();
        oversized
            .write_u16::<LittleEndian>(
                u16::try_from(MAX_SERVICE_SUMMARY_BYTES + 1).expect("test length fits"),
            )
            .unwrap();

        assert!(matches!(
            DiscoveryMessage::decode(&oversized),
            Err(DiscoveryWireError::OversizedPayload { .. })
        ));
    }

    #[test]
    fn known_service_summary_payload_trailing_bytes_are_rejected() {
        let mut envelope =
            ServiceSummaryEnvelope::header_sync(&header_summary()).expect("summary encodes");
        envelope.summary_bytes.push(0);

        assert!(matches!(
            services_message(vec![envelope.clone()]).encode(),
            Err(DiscoveryWireError::TrailingBytes)
        ));

        let node_id = secret_key().public();
        let mut encoded = vec![MSG_DISCOVERY_SERVICES];
        encoded.write_all(node_id.as_bytes()).unwrap();
        encoded.write_u64::<LittleEndian>(NOW + 30).unwrap();
        encoded.write_u16::<LittleEndian>(1).unwrap();
        encode_service_id(&envelope.service_id, &mut encoded).unwrap();
        encoded
            .write_u16::<LittleEndian>(envelope.summary_tag)
            .unwrap();
        encoded
            .write_u16::<LittleEndian>(
                u16::try_from(envelope.summary_bytes.len()).expect("test length fits"),
            )
            .unwrap();
        encoded.write_all(&envelope.summary_bytes).unwrap();

        assert!(matches!(
            DiscoveryMessage::decode(&encoded),
            Err(DiscoveryWireError::TrailingBytes)
        ));

        let mut block_envelope =
            ServiceSummaryEnvelope::block_sync(&block_sync_summary()).expect("summary encodes");
        block_envelope.summary_bytes.push(0);

        assert!(matches!(
            services_message(vec![block_envelope]).encode(),
            Err(DiscoveryWireError::TrailingBytes)
        ));
    }

    #[test]
    fn signature_verifies_for_author_and_fails_after_mutation() {
        let record = signed_record();
        record.verify(&context()).expect("record verifies");
        assert!(record
            .body
            .encode_for_signature()
            .expect("record pre-image encodes")
            .starts_with(ZAKURA_NODE_RECORD_SIG_DOMAIN));

        let mutations: Vec<
            Box<dyn FnOnce(&mut ZakuraNodeRecord, &mut DiscoveryRecordValidationContext)>,
        > = vec![
            Box::new(|record, _context| {
                record
                    .body
                    .direct_addrs
                    .push("198.51.100.1:8233".parse().unwrap())
            }),
            Box::new(|record, _context| record.body.services.push(service(9))),
            Box::new(|record, _context| record.body.zakura_protocol_min = 0),
            Box::new(|record, _context| record.body.zakura_protocol_max = 2),
            Box::new(|record, context| {
                record.body.network_id = ZakuraNetworkId::Mainnet;
                context.expected_network_id = ZakuraNetworkId::Mainnet;
            }),
            Box::new(|record, context| {
                record.body.chain_id[0] ^= 1;
                context.expected_chain_id = record.body.chain_id;
            }),
            Box::new(|record, _context| record.body.sequence += 1),
            Box::new(|record, _context| record.body.expires_at_unix_secs += 1),
        ];

        for mutate in mutations {
            let mut mutated = record.clone();
            let mut context = context();
            mutate(&mut mutated, &mut context);
            assert!(matches!(
                mutated.verify(&context),
                Err(DiscoveryRecordError::InvalidSignature)
            ));
        }

        let other_secret = secret_key();
        let mut mutated = record;
        mutated.body.node_id = other_secret.public();
        assert!(matches!(
            mutated.verify(&context()),
            Err(DiscoveryRecordError::InvalidSignature)
        ));
    }

    #[test]
    fn signature_fails_with_wrong_domain_or_record_format_version() {
        let secret_key = secret_key();
        let body = body(&secret_key);

        let wrong_domain = sign_record_with_prefix(
            body.clone(),
            &secret_key,
            b"zakura-other-record-v1",
            ZAKURA_NODE_RECORD_FORMAT_VERSION,
        );
        assert!(matches!(
            wrong_domain.verify(&context()),
            Err(DiscoveryRecordError::InvalidSignature)
        ));

        let wrong_version = sign_record_with_prefix(
            body,
            &secret_key,
            ZAKURA_NODE_RECORD_SIG_DOMAIN,
            ZAKURA_NODE_RECORD_FORMAT_VERSION + 1,
        );
        assert!(matches!(
            wrong_version.verify(&context()),
            Err(DiscoveryRecordError::InvalidSignature)
        ));
    }

    #[test]
    fn discovery_network_id_decode_matches_handshake_wire_codes() {
        for network_id in [
            ZakuraNetworkId::Mainnet,
            ZakuraNetworkId::Testnet,
            ZakuraNetworkId::Regtest,
            ZakuraNetworkId::Configured,
        ] {
            assert_eq!(
                network_id_from_code(network_id.code()).expect("network id code is valid"),
                network_id
            );
        }
    }

    #[test]
    fn record_validation_rejects_expired_wrong_network_wrong_chain_and_protocol() {
        let record = signed_record();

        let mut expired = record.clone();
        expired.body.expires_at_unix_secs = NOW - context().clock_skew_tolerance.as_secs() - 1;
        assert!(matches!(
            expired.verify(&context()),
            Err(DiscoveryRecordError::Expired)
        ));

        let mut wrong_network_context = context();
        wrong_network_context.expected_network_id = ZakuraNetworkId::Mainnet;
        assert!(matches!(
            record.verify(&wrong_network_context),
            Err(DiscoveryRecordError::WrongNetwork)
        ));

        let mut wrong_chain_context = context();
        wrong_chain_context.expected_chain_id[0] ^= 1;
        assert!(matches!(
            record.verify(&wrong_chain_context),
            Err(DiscoveryRecordError::WrongChain)
        ));

        let mut incompatible_context = context();
        incompatible_context.supported_protocol_min = 2;
        incompatible_context.supported_protocol_max = 2;
        assert!(matches!(
            record.verify(&incompatible_context),
            Err(DiscoveryRecordError::IncompatibleProtocol)
        ));
    }

    #[test]
    fn record_bounds_reject_too_many_addresses_and_services() {
        let secret_key = secret_key();
        let mut too_many_addrs = body(&secret_key);
        too_many_addrs.direct_addrs =
            vec!["192.0.2.1:8233".parse().unwrap(); MAX_DIRECT_ADDRS_PER_RECORD + 1];
        assert!(ZakuraNodeRecord::sign(too_many_addrs, &secret_key).is_err());

        let mut too_many_services = body(&secret_key);
        too_many_services.services = (0..=MAX_SERVICES_PER_RECORD).map(service).collect();
        assert!(ZakuraNodeRecord::sign(too_many_services, &secret_key).is_err());
    }

    #[test]
    fn response_and_query_bounds_are_enforced() {
        let records = vec![signed_record(); MAX_DISCOVERY_RECORDS_PER_RESPONSE + 1];
        assert!(DiscoveryMessage::Peers {
            records: records.clone()
        }
        .encode()
        .is_err());

        let wanted_services = (0..=MAX_SERVICES_PER_RECORD).map(service).collect();
        assert!(DiscoveryMessage::GetPeers {
            limit: 1,
            wanted_services,
            exclude_node_ids: Vec::new(),
        }
        .encode()
        .is_err());

        let wanted_services = (0..=MAX_GET_SERVICES_WANTED).map(service).collect();
        assert!(
            DiscoveryMessage::GetServices(GetServices { wanted_services })
                .encode()
                .is_err()
        );

        assert!(DiscoveryMessage::GetPeers {
            limit: u16::try_from(MAX_DISCOVERY_RECORDS_PER_RESPONSE + 1)
                .expect("test limit fits in u16"),
            wanted_services: Vec::new(),
            exclude_node_ids: Vec::new(),
        }
        .encode()
        .is_err());

        let too_many_summaries = vec![
            ServiceSummaryEnvelope {
                service_id: service(1),
                summary_tag: 999,
                summary_bytes: Vec::new(),
            };
            MAX_SERVICE_SUMMARIES_PER_RESPONSE + 1
        ];
        assert!(services_message(too_many_summaries).encode().is_err());

        let oversized_summary = ServiceSummaryEnvelope {
            service_id: service(1),
            summary_tag: 999,
            summary_bytes: vec![0; MAX_SERVICE_SUMMARY_BYTES + 1],
        };
        assert!(services_message(vec![oversized_summary]).encode().is_err());

        let large_summary = ServiceSummaryEnvelope {
            service_id: service(1),
            summary_tag: 999,
            summary_bytes: vec![0; MAX_SERVICE_SUMMARY_BYTES],
        };
        let total_oversized = vec![large_summary; 16];
        assert!(matches!(
            services_message(total_oversized).encode(),
            Err(DiscoveryWireError::OversizedPayload { .. })
        ));
    }

    #[test]
    fn known_summary_tags_must_match_their_service_ids() {
        let mismatched_header = ServiceSummaryEnvelope {
            service_id: ZakuraServiceId::discovery(),
            summary_tag: SUMMARY_TAG_HEADER_SYNC_V1,
            summary_bytes: {
                let mut bytes = Vec::new();
                encode_header_sync_summary(&header_summary(), &mut bytes)
                    .expect("test header summary encodes");
                bytes
            },
        };
        assert!(matches!(
            services_message(vec![mismatched_header.clone()]).encode(),
            Err(DiscoveryWireError::MismatchedSummaryService {
                summary_tag: SUMMARY_TAG_HEADER_SYNC_V1,
                ..
            })
        ));
        let mut mismatched_header_wire = vec![MSG_DISCOVERY_SERVICES];
        mismatched_header_wire
            .write_all(secret_key().public().as_bytes())
            .expect("test node id writes");
        mismatched_header_wire
            .write_u64::<LittleEndian>(NOW + 30)
            .expect("test expiry writes");
        mismatched_header_wire
            .write_u16::<LittleEndian>(1)
            .expect("test summary count writes");
        encode_service_id(&ZakuraServiceId::discovery(), &mut mismatched_header_wire)
            .expect("test service id writes");
        mismatched_header_wire
            .write_u16::<LittleEndian>(SUMMARY_TAG_HEADER_SYNC_V1)
            .expect("test summary tag writes");
        mismatched_header_wire
            .write_u16::<LittleEndian>(
                u16::try_from(mismatched_header.summary_bytes.len())
                    .expect("test summary length fits in u16"),
            )
            .expect("test summary length writes");
        mismatched_header_wire
            .write_all(&mismatched_header.summary_bytes)
            .expect("test summary payload writes");
        assert!(matches!(
            DiscoveryMessage::decode(&mismatched_header_wire),
            Err(DiscoveryWireError::MismatchedSummaryService {
                summary_tag: SUMMARY_TAG_HEADER_SYNC_V1,
                ..
            })
        ));

        let mismatched_discovery = ServiceSummaryEnvelope {
            service_id: ZakuraServiceId::header_sync(),
            summary_tag: SUMMARY_TAG_DISCOVERY_V1,
            summary_bytes: {
                let mut bytes = Vec::new();
                encode_discovery_summary(&discovery_summary(), &mut bytes)
                    .expect("test discovery summary encodes");
                bytes
            },
        };
        assert!(matches!(
            services_message(vec![mismatched_discovery]).encode(),
            Err(DiscoveryWireError::MismatchedSummaryService {
                summary_tag: SUMMARY_TAG_DISCOVERY_V1,
                ..
            })
        ));

        let mismatched_block = ServiceSummaryEnvelope {
            service_id: ZakuraServiceId::header_sync(),
            summary_tag: SUMMARY_TAG_BLOCK_SYNC_V1,
            summary_bytes: {
                let mut bytes = Vec::new();
                encode_block_sync_summary(&block_sync_summary(), &mut bytes)
                    .expect("test block summary encodes");
                bytes
            },
        };
        assert!(matches!(
            services_message(vec![mismatched_block]).encode(),
            Err(DiscoveryWireError::MismatchedSummaryService {
                summary_tag: SUMMARY_TAG_BLOCK_SYNC_V1,
                ..
            })
        ));
    }

    #[test]
    fn decode_rejects_too_many_records_services_and_excluded_ids_before_allocation() {
        let mut peers = vec![MSG_DISCOVERY_PEERS];
        peers
            .write_u16::<LittleEndian>(
                u16::try_from(MAX_DISCOVERY_RECORDS_PER_RESPONSE + 1)
                    .expect("test record count fits in u16"),
            )
            .unwrap();
        assert!(matches!(
            DiscoveryMessage::decode(&peers),
            Err(DiscoveryWireError::OversizedPayload { .. })
        ));

        let mut query = vec![MSG_DISCOVERY_GET_PEERS];
        query.write_u16::<LittleEndian>(1).unwrap();
        query
            .write_u16::<LittleEndian>(
                u16::try_from(MAX_SERVICES_PER_RECORD + 1).expect("test service count fits in u16"),
            )
            .unwrap();
        assert!(matches!(
            DiscoveryMessage::decode(&query),
            Err(DiscoveryWireError::OversizedPayload { .. })
        ));

        let mut query = vec![MSG_DISCOVERY_GET_PEERS];
        query.write_u16::<LittleEndian>(1).unwrap();
        query.write_u16::<LittleEndian>(0).unwrap();
        query
            .write_u16::<LittleEndian>(
                u16::try_from(MAX_DISCOVERY_EXCLUDED_NODE_IDS + 1)
                    .expect("test excluded count fits in u16"),
            )
            .unwrap();
        assert!(matches!(
            DiscoveryMessage::decode(&query),
            Err(DiscoveryWireError::OversizedPayload { .. })
        ));

        let mut service_query = vec![MSG_DISCOVERY_GET_SERVICES];
        service_query
            .write_u16::<LittleEndian>(
                u16::try_from(MAX_GET_SERVICES_WANTED + 1).expect("test service count fits in u16"),
            )
            .unwrap();
        assert!(matches!(
            DiscoveryMessage::decode(&service_query),
            Err(DiscoveryWireError::OversizedPayload { .. })
        ));

        let mut service_response = vec![MSG_DISCOVERY_SERVICES];
        service_response
            .write_all(secret_key().public().as_bytes())
            .unwrap();
        service_response
            .write_u64::<LittleEndian>(NOW + 30)
            .unwrap();
        service_response
            .write_u16::<LittleEndian>(
                u16::try_from(MAX_SERVICE_SUMMARIES_PER_RESPONSE + 1)
                    .expect("test summary count fits in u16"),
            )
            .unwrap();
        assert!(matches!(
            DiscoveryMessage::decode(&service_response),
            Err(DiscoveryWireError::OversizedPayload { .. })
        ));

        let mut oversized_summary = vec![MSG_DISCOVERY_SERVICES];
        oversized_summary
            .write_all(secret_key().public().as_bytes())
            .unwrap();
        oversized_summary
            .write_u64::<LittleEndian>(NOW + 30)
            .unwrap();
        oversized_summary.write_u16::<LittleEndian>(1).unwrap();
        encode_service_id(&service(1), &mut oversized_summary).unwrap();
        oversized_summary.write_u16::<LittleEndian>(999).unwrap();
        oversized_summary
            .write_u16::<LittleEndian>(
                u16::try_from(MAX_SERVICE_SUMMARY_BYTES + 1).expect("summary cap fits in u16"),
            )
            .unwrap();
        assert!(matches!(
            DiscoveryMessage::decode(&oversized_summary),
            Err(DiscoveryWireError::OversizedPayload { .. })
        ));
    }

    #[test]
    fn peers_response_cannot_encode_live_summaries_structurally() {
        let message = DiscoveryMessage::Peers {
            records: vec![signed_record()],
        };
        let encoded = message.encode().expect("peers message encodes");
        let decoded = DiscoveryMessage::decode(&encoded).expect("peers message decodes");

        match decoded {
            DiscoveryMessage::Peers { records } => assert_eq!(records.len(), 1),
            DiscoveryMessage::Services(_) => panic!("PEERS decoded as SERVICES"),
            _ => panic!("unexpected discovery message variant"),
        }
    }

    #[test]
    fn decode_rejects_malformed_record_body_counts_before_allocation() {
        let record = signed_record();
        let mut body_bytes = Vec::new();
        body_bytes
            .write_all(record.body.node_id.as_bytes())
            .unwrap();
        body_bytes
            .write_u16::<LittleEndian>(
                u16::try_from(MAX_DIRECT_ADDRS_PER_RECORD + 1)
                    .expect("test address count fits in u16"),
            )
            .unwrap();

        let mut encoded = vec![MSG_DISCOVERY_HELLO];
        encoded
            .write_u32::<LittleEndian>(
                u32::try_from(body_bytes.len()).expect("test body length fits in u32"),
            )
            .unwrap();
        encoded.write_all(&body_bytes).unwrap();
        encoded.write_all(&record.signature.to_bytes()).unwrap();

        assert!(matches!(
            DiscoveryMessage::decode(&encoded),
            Err(DiscoveryWireError::OversizedPayload { .. })
        ));

        let record = signed_record();
        let mut body_bytes = Vec::new();
        body_bytes
            .write_all(record.body.node_id.as_bytes())
            .unwrap();
        body_bytes.write_u16::<LittleEndian>(0).unwrap();
        body_bytes
            .write_u16::<LittleEndian>(
                u16::try_from(MAX_SERVICES_PER_RECORD + 1).expect("test service count fits in u16"),
            )
            .unwrap();

        let mut encoded = vec![MSG_DISCOVERY_HELLO];
        encoded
            .write_u32::<LittleEndian>(
                u32::try_from(body_bytes.len()).expect("test body length fits in u32"),
            )
            .unwrap();
        encoded.write_all(&body_bytes).unwrap();
        encoded.write_all(&record.signature.to_bytes()).unwrap();

        assert!(matches!(
            DiscoveryMessage::decode(&encoded),
            Err(DiscoveryWireError::OversizedPayload { .. })
        ));
    }

    #[test]
    fn signature_fails_when_signed_vectors_are_reordered() {
        let record = signed_record();
        let signed_bytes = record
            .body
            .encode_for_signature()
            .expect("record pre-image encodes");

        let mut reordered_services = record.clone();
        reordered_services.body.services.swap(0, 1);
        assert_ne!(
            reordered_services
                .body
                .encode_for_signature()
                .expect("reordered services pre-image encodes"),
            signed_bytes
        );
        assert!(matches!(
            reordered_services.verify(&context()),
            Err(DiscoveryRecordError::InvalidSignature)
        ));

        let mut reordered_addrs = record;
        reordered_addrs.body.direct_addrs.swap(0, 1);
        assert_ne!(
            reordered_addrs
                .body
                .encode_for_signature()
                .expect("reordered addrs pre-image encodes"),
            signed_bytes
        );
        assert!(matches!(
            reordered_addrs.verify(&context()),
            Err(DiscoveryRecordError::InvalidSignature)
        ));
    }

    #[test]
    fn far_future_expiry_and_clock_skew_edges_are_enforced() {
        let mut record = signed_record();

        record.body.expires_at_unix_secs =
            NOW + context().max_record_ttl.as_secs() + context().clock_skew_tolerance.as_secs();
        let signing_secret = secret_key();
        record = ZakuraNodeRecord::sign(
            ZakuraNodeRecordBody {
                node_id: signing_secret.public(),
                ..record.body
            },
            &signing_secret,
        )
        .expect("record signs");
        record.verify(&context()).expect("skew edge is accepted");

        let signing_secret = secret_key();
        let mut too_far = body(&signing_secret);
        too_far.expires_at_unix_secs =
            NOW + context().max_record_ttl.as_secs() + context().clock_skew_tolerance.as_secs() + 1;
        let too_far = ZakuraNodeRecord::sign(too_far, &signing_secret).expect("record signs");
        assert!(matches!(
            too_far.verify(&context()),
            Err(DiscoveryRecordError::FarFutureExpiry)
        ));

        let signing_secret = secret_key();
        let mut just_expired = body(&signing_secret);
        just_expired.expires_at_unix_secs = NOW - context().clock_skew_tolerance.as_secs();
        let just_expired =
            ZakuraNodeRecord::sign(just_expired, &signing_secret).expect("record signs");
        just_expired
            .verify(&context())
            .expect("just expired within skew is accepted");
    }

    #[test]
    fn decode_rejects_trailing_and_unknown_bytes() {
        let mut encoded = DiscoveryMessage::Hello {
            record: signed_record(),
        }
        .encode()
        .expect("message encodes");
        encoded.push(0);
        assert!(matches!(
            DiscoveryMessage::decode(&encoded),
            Err(DiscoveryWireError::TrailingBytes)
        ));

        assert!(matches!(
            DiscoveryMessage::decode(&[99]),
            Err(DiscoveryWireError::InvalidMessageType(99))
        ));
    }

    #[test]
    fn discovery_book_imports_valid_record() {
        let mut book = ZakuraDiscoveryBook::default();
        let source = secret_key().public();
        let record = signed_record();
        let node_id = record.body.node_id;

        assert_eq!(
            book.import_record(record.clone(), Some(source), NOW, &context())
                .expect("valid record imports"),
            ImportOutcome::Added
        );

        let entry = book.get(&node_id).expect("entry was inserted");
        assert_eq!(entry.record(), &record);
        assert_eq!(entry.source(), Some(source));
        assert_eq!(entry.last_seen(), NOW);
        assert!(!entry.is_static());
    }

    #[test]
    fn discovery_book_rejects_invalid_signature_expired_wrong_network_and_wrong_chain() {
        let mut book = ZakuraDiscoveryBook::default();

        let mut invalid_signature = signed_record();
        invalid_signature.body.sequence += 1;
        assert!(matches!(
            book.import_record(invalid_signature, None, NOW, &context()),
            Err(DiscoveryBookError::Record(
                DiscoveryRecordError::InvalidSignature
            ))
        ));

        let signing_secret = secret_key();
        let mut expired = body(&signing_secret);
        expired.expires_at_unix_secs = NOW - context().clock_skew_tolerance.as_secs() - 1;
        let expired = ZakuraNodeRecord::sign(expired, &signing_secret).expect("record signs");
        assert!(matches!(
            book.import_record(expired, None, NOW, &context()),
            Err(DiscoveryBookError::Record(DiscoveryRecordError::Expired))
        ));

        let mut wrong_network_context = context();
        wrong_network_context.expected_network_id = ZakuraNetworkId::Mainnet;
        assert!(matches!(
            book.import_record(signed_record(), None, NOW, &wrong_network_context),
            Err(DiscoveryBookError::Record(
                DiscoveryRecordError::WrongNetwork
            ))
        ));

        let mut wrong_chain_context = context();
        wrong_chain_context.expected_chain_id[0] ^= 1;
        assert!(matches!(
            book.import_record(signed_record(), None, NOW, &wrong_chain_context),
            Err(DiscoveryBookError::Record(DiscoveryRecordError::WrongChain))
        ));
    }

    #[test]
    fn discovery_book_rejects_self_record_no_usable_address_and_far_future_expiry() {
        let local_secret = secret_key();
        let local_record = record_with_secret(&local_secret, 1, service(1));
        let mut book = ZakuraDiscoveryBook::with_local_node_id(
            ZakuraDiscoveryBookLimits::default(),
            local_secret.public(),
        );

        assert!(matches!(
            book.import_record(local_record.clone(), None, NOW, &context()),
            Err(DiscoveryBookError::SelfRecord)
        ));
        assert!(book.get(&local_secret.public()).is_none());

        let signing_secret = secret_key();
        let mut no_addr = body(&signing_secret);
        no_addr.direct_addrs = Vec::new();
        let no_addr = ZakuraNodeRecord::sign(no_addr, &signing_secret).expect("record signs");
        assert!(matches!(
            book.import_record(no_addr, None, NOW, &context()),
            Err(DiscoveryBookError::NoUsableDirectAddress)
        ));

        let signing_secret = secret_key();
        let mut too_far = body(&signing_secret);
        too_far.expires_at_unix_secs =
            NOW + context().max_record_ttl.as_secs() + context().clock_skew_tolerance.as_secs() + 1;
        let too_far = ZakuraNodeRecord::sign(too_far, &signing_secret).expect("record signs");
        assert!(matches!(
            book.import_record(too_far, None, NOW, &context()),
            Err(DiscoveryBookError::Record(
                DiscoveryRecordError::FarFutureExpiry
            ))
        ));
    }

    #[test]
    fn discovery_book_rejects_non_dialable_direct_addresses() {
        let bad_addrs = [
            SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 8233),
            SocketAddr::new(IpAddr::V6(Ipv6Addr::LOCALHOST), 8233),
            SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 8233),
            SocketAddr::new(IpAddr::V6(Ipv6Addr::UNSPECIFIED), 8233),
            SocketAddr::new(IpAddr::V4(Ipv4Addr::new(224, 0, 0, 1)), 8233),
            SocketAddr::new(IpAddr::V6(Ipv6Addr::new(0xff02, 0, 0, 0, 0, 0, 0, 1)), 8233),
            SocketAddr::new(IpAddr::V4(Ipv4Addr::BROADCAST), 8233),
            SocketAddr::new(IpAddr::V4(Ipv4Addr::new(169, 254, 1, 1)), 8233),
            SocketAddr::new(IpAddr::V6(Ipv6Addr::new(0xfe80, 0, 0, 0, 0, 0, 0, 1)), 8233),
            SocketAddr::new(IpAddr::V4(Ipv4Addr::new(192, 0, 2, 20)), 0),
        ];

        for (index, bad_addr) in bad_addrs.into_iter().enumerate() {
            let mut book = ZakuraDiscoveryBook::default();
            let record = signed_record_with_addrs(
                u64::try_from(index).expect("small test index fits in u64"),
                service(index),
                vec![test_addr(20), bad_addr],
            );

            assert!(matches!(
                book.import_record(record, None, NOW, &context()),
                Err(DiscoveryBookError::NonDialableDirectAddress { addr }) if addr == bad_addr
            ));
            assert!(book.is_empty());
        }
    }

    #[test]
    fn discovery_book_peer_import_rejects_private_and_internal_direct_addresses() {
        // A signed record only proves control of the node key, not ownership of the advertised
        // direct addresses. Untrusted gossip must therefore be restricted to globally routable
        // targets; otherwise an authenticated discovery peer could seed signed records pointing at
        // the honest node's private/internal network and turn the candidate dialer into an internal
        // SSRF/scanner. RFC 1918 private, RFC 6598 shared (CGNAT), and RFC 4193 unique-local ranges
        // are neither loopback nor link-local, so the original filter let them through.
        let bad_addrs = [
            SocketAddr::new(IpAddr::V4(Ipv4Addr::new(10, 0, 0, 5)), 8233),
            SocketAddr::new(IpAddr::V4(Ipv4Addr::new(172, 16, 5, 5)), 8233),
            SocketAddr::new(IpAddr::V4(Ipv4Addr::new(192, 168, 1, 1)), 8233),
            SocketAddr::new(IpAddr::V4(Ipv4Addr::new(100, 64, 0, 1)), 8233),
            SocketAddr::new(IpAddr::V6(Ipv6Addr::new(0xfc00, 0, 0, 0, 0, 0, 0, 1)), 8233),
            SocketAddr::new(
                IpAddr::V6(Ipv6Addr::new(0xfd12, 0x3456, 0, 0, 0, 0, 0, 1)),
                8233,
            ),
        ];

        for (index, bad_addr) in bad_addrs.into_iter().enumerate() {
            let mut book = ZakuraDiscoveryBook::default();
            // Pair the malicious target with a genuinely routable address to prove the whole
            // record is rejected, not just dropped down to its usable addresses.
            let record = signed_record_with_addrs(
                u64::try_from(index).expect("small test index fits in u64"),
                service(index),
                vec![test_addr(30), bad_addr],
            );

            assert!(
                matches!(
                    book.import_record(record, Some(secret_key().public()), NOW, &context()),
                    Err(DiscoveryBookError::NonDialableDirectAddress { addr }) if addr == bad_addr
                ),
                "untrusted gossip must reject internal direct address {bad_addr}"
            );
            assert!(book.is_empty());
        }

        // A genuinely globally routable gossip target still imports.
        let mut book = ZakuraDiscoveryBook::default();
        let public_record = signed_record_with_addrs(99, service(99), vec![test_addr(40)]);
        assert_eq!(
            book.import_record(public_record, Some(secret_key().public()), NOW, &context())
                .expect("public gossip record imports"),
            ImportOutcome::Added
        );

        // Trusted/static configuration is unchanged: operators may still configure private targets
        // for LAN/regtest deployments.
        let mut static_book = ZakuraDiscoveryBook::default();
        let private_static = signed_record_with_addrs(
            1,
            service(1),
            vec![SocketAddr::new(
                IpAddr::V4(Ipv4Addr::new(192, 168, 1, 1)),
                8233,
            )],
        );
        assert_eq!(
            static_book
                .import_static_record(private_static, NOW, &context())
                .expect("configured static private record imports"),
            ImportOutcome::Added
        );
    }

    #[test]
    fn discovery_book_gossiped_public_third_party_record_is_not_dialable() {
        let mut book = ZakuraDiscoveryBook::default();
        let gossip_source = secret_key().public();
        let public_target = signed_record_with_addrs(
            1,
            service(1),
            vec![SocketAddr::new(
                IpAddr::V4(Ipv4Addr::new(93, 184, 216, 34)),
                8233,
            )],
        );
        let target_id = public_target.body.node_id;

        assert_eq!(
            book.import_record(public_target.clone(), Some(gossip_source), NOW, &context())
                .expect("valid public third-party gossip record still imports"),
            ImportOutcome::Added
        );
        assert_eq!(
            book.get(&target_id).expect("entry is stored").record(),
            &public_target
        );

        assert!(
            book.dial_candidates(
                10,
                &[service(1)],
                DialCandidateExclusions {
                    connected_node_ids: &[],
                    in_flight_node_ids: &[],
                },
                NOW,
                (
                    DEFAULT_DISCOVERY_DIAL_BACKOFF_BASE,
                    DEFAULT_DISCOVERY_DIAL_BACKOFF_MAX,
                ),
                &mut StdRng::seed_from_u64(7),
            )
            .is_empty(),
            "unconfirmed third-party public gossip must not drive native dials"
        );

        assert_eq!(
            book.import_record(public_target.clone(), Some(target_id), NOW + 1, &context())
                .expect("first-party self-record confirmation imports"),
            ImportOutcome::MetadataUpdated
        );
        assert_eq!(
            book.dial_candidates(
                10,
                &[service(1)],
                DialCandidateExclusions {
                    connected_node_ids: &[],
                    in_flight_node_ids: &[],
                },
                NOW + 1,
                (
                    DEFAULT_DISCOVERY_DIAL_BACKOFF_BASE,
                    DEFAULT_DISCOVERY_DIAL_BACKOFF_MAX,
                ),
                &mut StdRng::seed_from_u64(7),
            ),
            vec![candidate_for(&public_target, false)]
        );
    }

    #[test]
    fn discovery_book_static_import_allows_loopback_direct_address() {
        let mut book = ZakuraDiscoveryBook::default();
        let loopback_addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 8233);
        let record = signed_record_with_addrs(1, service(1), vec![loopback_addr]);
        let node_id = record.body.node_id;

        assert_eq!(
            book.import_static_record(record.clone(), NOW, &context())
                .expect("configured static loopback record imports"),
            ImportOutcome::Added
        );

        let entry = book
            .get(&node_id)
            .expect("static loopback entry was stored");
        assert!(entry.is_static());
        assert_eq!(entry.record(), &record);
    }

    #[test]
    fn discovery_book_peer_import_still_rejects_loopback_direct_address() {
        let mut book = ZakuraDiscoveryBook::default();
        let loopback_addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 8233);
        let record = signed_record_with_addrs(1, service(1), vec![loopback_addr]);

        assert!(matches!(
            book.import_record(record, Some(secret_key().public()), NOW, &context()),
            Err(DiscoveryBookError::NonDialableDirectAddress { addr }) if addr == loopback_addr
        ));
        assert!(book.is_empty());
    }

    #[test]
    fn discovery_book_static_import_rejects_unspecified_and_port_zero_addresses() {
        let bad_addrs = [
            SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 8233),
            SocketAddr::new(IpAddr::V6(Ipv6Addr::UNSPECIFIED), 8233),
            SocketAddr::new(IpAddr::V4(Ipv4Addr::new(224, 0, 0, 1)), 8233),
            SocketAddr::new(IpAddr::V6(Ipv6Addr::new(0xff02, 0, 0, 0, 0, 0, 0, 1)), 8233),
            SocketAddr::new(IpAddr::V4(Ipv4Addr::BROADCAST), 8233),
            SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0),
        ];

        for (index, bad_addr) in bad_addrs.into_iter().enumerate() {
            let mut book = ZakuraDiscoveryBook::default();
            let record = signed_record_with_addrs(
                u64::try_from(index).expect("small test index fits in u64"),
                service(index),
                vec![bad_addr],
            );

            assert!(matches!(
                book.import_static_record(record, NOW, &context()),
                Err(DiscoveryBookError::NonDialableDirectAddress { addr }) if addr == bad_addr
            ));
            assert!(book.is_empty());
        }
    }

    #[test]
    fn discovery_book_inserts_static_bootstrap_candidate_without_gossiping_it() {
        let mut book = ZakuraDiscoveryBook::default();
        let node_id = secret_key().public();
        let loopback_addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 8233);
        let node_addr = NodeAddr::new(node_id).with_direct_addresses([loopback_addr]);

        book.insert_static_candidate(node_addr, NOW)
            .expect("configured static loopback candidate imports");

        let mut rng = StdRng::seed_from_u64(7);
        assert!(book.sample_peers(10, &[], &[], NOW, &mut rng).is_empty());
        assert_eq!(
            book.dial_candidates(
                10,
                &[],
                DialCandidateExclusions {
                    connected_node_ids: &[],
                    in_flight_node_ids: &[],
                },
                NOW,
                (
                    DEFAULT_DISCOVERY_DIAL_BACKOFF_BASE,
                    DEFAULT_DISCOVERY_DIAL_BACKOFF_MAX,
                ),
                &mut rng,
            ),
            vec![ZakuraDiscoveryDialCandidate {
                node_id,
                direct_addrs: vec![loopback_addr],
                is_static: true,
            }]
        );
    }

    #[test]
    fn discovery_book_static_bootstrap_candidate_rejects_unusable_addresses() {
        let bad_addrs = [
            SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 8233),
            SocketAddr::new(IpAddr::V6(Ipv6Addr::UNSPECIFIED), 8233),
            SocketAddr::new(IpAddr::V4(Ipv4Addr::new(224, 0, 0, 1)), 8233),
            SocketAddr::new(IpAddr::V6(Ipv6Addr::new(0xff02, 0, 0, 0, 0, 0, 0, 1)), 8233),
            SocketAddr::new(IpAddr::V4(Ipv4Addr::BROADCAST), 8233),
            SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0),
        ];

        for bad_addr in bad_addrs {
            let mut book = ZakuraDiscoveryBook::default();
            let node_addr = NodeAddr::new(secret_key().public()).with_direct_addresses([bad_addr]);

            assert!(matches!(
                book.insert_static_candidate(node_addr, NOW),
                Err(DiscoveryBookError::NonDialableDirectAddress { addr }) if addr == bad_addr
            ));
            assert!(book.is_empty());
        }
    }

    #[test]
    fn discovery_book_sequence_rules_keep_newest_record() {
        let mut book = ZakuraDiscoveryBook::default();
        let secret = secret_key();
        let newer = record_with_secret(&secret, 10, service(1));
        let older = record_with_secret(&secret, 9, service(2));
        let equal = record_with_secret(&secret, 10, service(3));
        let node_id = newer.body.node_id;

        assert_eq!(
            book.import_record(newer.clone(), None, NOW, &context())
                .unwrap(),
            ImportOutcome::Added
        );
        assert_eq!(
            book.import_record(older, Some(secret_key().public()), NOW + 1, &context())
                .unwrap(),
            ImportOutcome::IgnoredOlder
        );
        assert_eq!(
            book.get(&node_id).unwrap().record().body.services,
            newer.body.services
        );

        let equal_source = secret_key().public();
        assert_eq!(
            book.import_record(equal, Some(equal_source), NOW + 2, &context())
                .unwrap(),
            ImportOutcome::MetadataUpdated
        );
        let entry = book.get(&node_id).unwrap();
        assert_eq!(entry.record().body.services, newer.body.services);
        assert_eq!(entry.source(), Some(equal_source));
        assert_eq!(entry.last_seen(), NOW + 2);

        let newest = record_with_secret(&secret, 11, service(4));
        assert_eq!(
            book.import_record(newest.clone(), None, NOW + 3, &context())
                .unwrap(),
            ImportOutcome::Updated
        );
        assert_eq!(book.get(&node_id).unwrap().record(), &newest);
    }

    #[test]
    fn discovery_book_import_batch_enforces_per_response_cap() {
        let mut book = ZakuraDiscoveryBook::new(ZakuraDiscoveryBookLimits {
            max_imported_records_per_response: 2,
            ..ZakuraDiscoveryBookLimits::default()
        });
        let records = (1u8..=4)
            .map(|index| {
                signed_record_with(u64::from(index), service(index as usize), test_addr(index))
            })
            .collect::<Vec<_>>();

        let outcome = book.import_records(records, None, NOW, &context());

        assert_eq!(outcome.attempted, 2);
        assert_eq!(outcome.added, 2);
        assert_eq!(outcome.dropped_for_limit, 2);
        assert_eq!(book.discovered_len(), 2);
    }

    #[test]
    fn discovery_book_total_capacity_and_eviction_order_are_enforced() {
        let mut book = small_book(2);

        let expired_secret = secret_key();
        let mut expired_body = body(&expired_secret);
        expired_body.sequence = 1;
        expired_body.expires_at_unix_secs = NOW - 1;
        let expired_context = DiscoveryRecordValidationContext {
            current_unix_secs: NOW - context().clock_skew_tolerance.as_secs(),
            ..context()
        };
        let expired = ZakuraNodeRecord::sign(expired_body, &expired_secret).expect("record signs");
        let expired_id = expired.body.node_id;
        book.import_record(
            expired,
            None,
            NOW - context().clock_skew_tolerance.as_secs(),
            &expired_context,
        )
        .expect("expired-later record imports before eviction time");

        let failed = signed_record_with(2, service(2), test_addr(2));
        let failed_id = failed.body.node_id;
        book.import_record(failed, None, NOW, &context()).unwrap();
        for _ in 0..HIGH_DIAL_FAILURE_COUNT {
            book.mark_dial_failure(&failed_id, NOW);
        }

        let survivor = signed_record_with(3, service(3), test_addr(3));
        let survivor_id = survivor.body.node_id;
        book.import_record(survivor, None, NOW, &context()).unwrap();

        assert!(book.get(&expired_id).is_none());
        assert!(book.get(&failed_id).is_some());
        assert!(book.get(&survivor_id).is_some());

        let replacement = signed_record_with(4, service(4), test_addr(4));
        let replacement_id = replacement.body.node_id;
        book.import_record(replacement, None, NOW, &context())
            .unwrap();

        assert!(book.get(&failed_id).is_none());
        assert!(book.get(&survivor_id).is_some());
        assert!(book.get(&replacement_id).is_some());
        assert_eq!(book.discovered_len(), 2);
    }

    #[test]
    fn discovery_book_static_entries_survive_eviction_storm() {
        let mut book = small_book(1);
        let static_record = signed_record_with(1, service(1), test_addr(1));
        let static_id = static_record.body.node_id;
        book.import_static_record(static_record, NOW, &context())
            .expect("static record imports");

        for index in 2..=10 {
            book.import_record(
                signed_record_with(index.into(), service(index as usize), test_addr(index)),
                Some(secret_key().public()),
                NOW,
                &context(),
            )
            .expect("flood record imports");
        }

        assert!(book
            .get(&static_id)
            .expect("static entry remains")
            .is_static());
        assert_eq!(book.discovered_len(), 1);
        assert_eq!(book.len(), 2);
    }

    #[test]
    fn discovery_book_evicts_least_recently_successful_entry() {
        let mut book = small_book(2);
        let older_success = signed_record_with(1, service(1), test_addr(1));
        let recent_success = signed_record_with(2, service(2), test_addr(2));
        let replacement = signed_record_with(3, service(3), test_addr(3));
        let older_success_id = older_success.body.node_id;
        let recent_success_id = recent_success.body.node_id;
        let replacement_id = replacement.body.node_id;

        let persisted = [older_success, recent_success, replacement]
            .into_iter()
            .enumerate()
            .map(|(index, record)| ZakuraDiscoveryPersistedEntry {
                record,
                source: None,
                is_static: false,
                last_seen: NOW,
                last_dial_attempt: None,
                last_success: Some(
                    NOW + u64::try_from(index).expect("small test index fits in u64") + 1,
                ),
                failure_count: 0,
            });

        let outcome = book.import_persisted_entries(persisted, NOW, &context());

        assert_eq!(outcome.added, 3);
        assert!(book.get(&older_success_id).is_none());
        assert!(book.get(&recent_success_id).is_some());
        assert!(book.get(&replacement_id).is_some());
        assert_eq!(book.discovered_len(), 2);
    }

    #[test]
    fn discovery_book_samples_are_bounded_excluded_and_service_filtered() {
        let mut book = ZakuraDiscoveryBook::default();
        let wanted = service(1);
        let other = service(2);
        let matching_a = signed_record_with(1, wanted.clone(), test_addr(1));
        let matching_b = signed_record_with(2, wanted.clone(), test_addr(2));
        let matching_c = signed_record_with(3, wanted.clone(), test_addr(3));
        let non_matching = signed_record_with(4, other, test_addr(4));
        let excluded = matching_a.body.node_id;
        book.import_record(matching_a, None, NOW, &context())
            .unwrap();
        book.import_record(matching_b, None, NOW, &context())
            .unwrap();
        book.import_record(matching_c, None, NOW, &context())
            .unwrap();
        book.import_record(non_matching, None, NOW, &context())
            .unwrap();

        let mut rng = StdRng::seed_from_u64(7);
        let sample =
            book.sample_peers(1, std::slice::from_ref(&wanted), &[excluded], NOW, &mut rng);
        let sample_ids: HashSet<_> = sample.iter().map(|record| record.body.node_id).collect();

        assert_eq!(sample.len(), 1);
        assert!(!sample_ids.contains(&excluded));
        assert!(sample
            .iter()
            .all(|record| record.body.services.contains(&wanted)));
    }

    #[test]
    fn discovery_book_samples_skip_expired_records() {
        let mut book = ZakuraDiscoveryBook::default();
        let record = signed_record_with(1, service(1), test_addr(1));
        book.import_record(record, None, NOW, &context()).unwrap();

        let mut rng = StdRng::seed_from_u64(7);
        let sample = book.sample_peers(10, &[], &[], NOW + 61, &mut rng);

        assert!(sample.is_empty());
    }

    #[test]
    fn discovery_book_dial_metadata_updates_candidates_without_blacklisting() {
        let mut book = ZakuraDiscoveryBook::default();
        let preferred = signed_record_with(1, service(1), test_addr(1));
        let failed = signed_record_with(2, service(1), test_addr(2));
        let failed_id = failed.body.node_id;
        let preferred_id = preferred.body.node_id;

        import_confirmed_record(&mut book, failed.clone()).unwrap();
        import_confirmed_record(&mut book, preferred.clone()).unwrap();
        book.mark_dial_attempt(&failed_id, NOW);
        book.mark_dial_failure(&failed_id, NOW);
        book.mark_dial_success(&preferred_id, NOW + 1);

        let mut rng = StdRng::seed_from_u64(7);
        let candidates = book.dial_candidates(
            10,
            &[service(1)],
            DialCandidateExclusions {
                connected_node_ids: &[],
                in_flight_node_ids: &[],
            },
            NOW + 1,
            (
                DEFAULT_DISCOVERY_DIAL_BACKOFF_BASE,
                DEFAULT_DISCOVERY_DIAL_BACKOFF_MAX,
            ),
            &mut rng,
        );
        assert_eq!(candidates, vec![candidate_for(&preferred, false)]);
        assert_eq!(book.get(&failed_id).unwrap().failure_count(), 1);

        let candidates = book.dial_candidates(
            10,
            &[service(1)],
            DialCandidateExclusions {
                connected_node_ids: &[],
                in_flight_node_ids: &[],
            },
            NOW + 60,
            (
                DEFAULT_DISCOVERY_DIAL_BACKOFF_BASE,
                DEFAULT_DISCOVERY_DIAL_BACKOFF_MAX,
            ),
            &mut rng,
        );
        assert_eq!(
            candidates,
            vec![
                candidate_for(&preferred, false),
                candidate_for(&failed, false)
            ]
        );
    }

    #[test]
    fn discovery_book_randomizes_comparable_non_static_dial_candidates() {
        let mut book = ZakuraDiscoveryBook::default();
        let records = (1u8..=32)
            .map(|index| signed_record_with(index.into(), service(1), test_addr(index)))
            .collect::<Vec<_>>();

        for record in &records {
            import_confirmed_record(&mut book, record.clone()).expect("test record imports");
        }

        let mut deterministic = records
            .iter()
            .map(|record| candidate_for(record, false))
            .collect::<Vec<_>>();
        deterministic.sort_by_key(|candidate| node_id_sort_key(&candidate.node_id));

        let mut rng = StdRng::seed_from_u64(11);
        let sampled = book.dial_candidates(
            records.len(),
            &[service(1)],
            DialCandidateExclusions {
                connected_node_ids: &[],
                in_flight_node_ids: &[],
            },
            NOW,
            (
                DEFAULT_DISCOVERY_DIAL_BACKOFF_BASE,
                DEFAULT_DISCOVERY_DIAL_BACKOFF_MAX,
            ),
            &mut rng,
        );
        let mut other_rng = StdRng::seed_from_u64(12);
        let other_sampled = book.dial_candidates(
            records.len(),
            &[service(1)],
            DialCandidateExclusions {
                connected_node_ids: &[],
                in_flight_node_ids: &[],
            },
            NOW,
            (
                DEFAULT_DISCOVERY_DIAL_BACKOFF_BASE,
                DEFAULT_DISCOVERY_DIAL_BACKOFF_MAX,
            ),
            &mut other_rng,
        );

        let sampled_ids: HashSet<_> = sampled.iter().map(|candidate| candidate.node_id).collect();
        assert_eq!(sampled_ids.len(), records.len());
        assert_eq!(
            sampled_ids,
            deterministic
                .iter()
                .map(|candidate| candidate.node_id)
                .collect::<HashSet<_>>()
        );
        assert_ne!(sampled, deterministic);
        assert_ne!(sampled, other_sampled);
    }

    #[test]
    fn discovery_book_dial_backoff_uses_configured_bounds() {
        let mut book = ZakuraDiscoveryBook::default();
        let failed = signed_record_with(1, service(1), test_addr(1));
        let failed_id = failed.body.node_id;

        import_confirmed_record(&mut book, failed.clone()).unwrap();
        book.mark_dial_attempt(&failed_id, NOW);
        book.mark_dial_failure(&failed_id, NOW);

        assert!(book
            .dial_candidates(
                10,
                &[service(1)],
                DialCandidateExclusions {
                    connected_node_ids: &[],
                    in_flight_node_ids: &[],
                },
                NOW + 29,
                (Duration::from_secs(30), Duration::from_secs(300)),
                &mut StdRng::seed_from_u64(7),
            )
            .is_empty());
        let mut rng = StdRng::seed_from_u64(7);
        assert_eq!(
            book.dial_candidates(
                10,
                &[service(1)],
                DialCandidateExclusions {
                    connected_node_ids: &[],
                    in_flight_node_ids: &[],
                },
                NOW + 30,
                (Duration::from_secs(30), Duration::from_secs(300)),
                &mut rng,
            ),
            vec![candidate_for(&failed, false)]
        );
    }

    #[test]
    fn discovery_book_short_lived_exchange_backoff_is_non_punitive() {
        let mut book = ZakuraDiscoveryBook::default();
        let exchanged = signed_record_with(1, service(1), test_addr(1));
        let exchanged_id = exchanged.body.node_id;

        import_confirmed_record(&mut book, exchanged.clone()).unwrap();
        book.mark_dial_success(&exchanged_id, NOW);
        book.mark_short_lived_exchange(&exchanged_id, NOW);

        let entry = book.get(&exchanged_id).unwrap();
        assert_eq!(entry.failure_count(), 0);
        assert_eq!(entry.last_short_lived_exchange(), Some(NOW));

        assert!(book
            .dial_candidates(
                10,
                &[service(1)],
                DialCandidateExclusions {
                    connected_node_ids: &[],
                    in_flight_node_ids: &[],
                },
                NOW + 29,
                (Duration::from_secs(30), Duration::from_secs(300)),
                &mut StdRng::seed_from_u64(7),
            )
            .is_empty());
        assert_eq!(
            book.dial_candidates(
                10,
                &[service(1)],
                DialCandidateExclusions {
                    connected_node_ids: &[],
                    in_flight_node_ids: &[],
                },
                NOW + 30,
                (Duration::from_secs(30), Duration::from_secs(300)),
                &mut StdRng::seed_from_u64(7),
            ),
            vec![candidate_for(&exchanged, false)]
        );
    }

    #[test]
    fn discovery_book_services_and_persistence_hooks_revalidate_records() {
        let mut book = ZakuraDiscoveryBook::default();
        let record = signed_record_with(1, service(9), test_addr(9));
        let node_id = record.body.node_id;
        import_confirmed_record(&mut book, record.clone()).unwrap();
        book.mark_dial_attempt(&node_id, NOW + 1);
        book.mark_dial_failure(&node_id, NOW + 1);

        assert_eq!(book.services_for(&node_id), Some(vec![service(9)]));

        let mut persisted = book.persisted_entries();
        persisted.push(ZakuraDiscoveryPersistedEntry {
            record: {
                let mut invalid = record.clone();
                invalid.body.sequence += 1;
                invalid
            },
            source: None,
            is_static: false,
            last_seen: NOW,
            last_dial_attempt: None,
            last_success: None,
            failure_count: 0,
        });

        let mut reloaded = ZakuraDiscoveryBook::default();
        let outcome = reloaded.import_persisted_entries(persisted, NOW, &context());

        assert_eq!(outcome.added, 1);
        assert_eq!(outcome.rejected, 1);
        let reloaded_entry = reloaded
            .get(&node_id)
            .expect("valid persisted entry reloads");
        assert_eq!(reloaded_entry.record(), &record);
        assert_eq!(reloaded_entry.last_dial_attempt(), Some(NOW + 1));
        assert_eq!(reloaded_entry.failure_count(), 1);
    }

    #[test]
    fn self_record_includes_local_node_addresses_services_and_verifies() {
        let secret = secret_key();
        let node_id = secret.public();
        let (_connected_tx, connected_rx) = watch::channel(Vec::new());
        let handle = discovery_handle_at(
            local_config_with(
                secret,
                vec![test_addr(42)],
                vec![
                    ZakuraServiceId::legacy_requests(),
                    ZakuraServiceId::discovery(),
                ],
            ),
            ZakuraDiscoveryConfig::default(),
            connected_rx,
            NOW,
        );

        let record = handle.current_self_record();

        assert_eq!(record.body.node_id, node_id);
        assert_eq!(record.body.direct_addrs, vec![test_addr(42)]);
        assert_eq!(
            record.body.services,
            vec![
                ZakuraServiceId::discovery(),
                ZakuraServiceId::legacy_requests()
            ]
        );
        record.verify(&context()).expect("self-record verifies");
    }

    #[tokio::test]
    async fn self_record_sequence_increases_when_services_change() {
        let (_connected_tx, connected_rx) = watch::channel(Vec::new());
        let handle = discovery_handle_at(
            local_config_with(secret_key(), vec![test_addr(43)], vec![service(1)]),
            ZakuraDiscoveryConfig::default(),
            connected_rx,
            NOW,
        );
        let initial = handle.current_self_record();

        let updated = handle
            .update_advertised_services(vec![service(2)])
            .await
            .expect("updated self-record signs");

        assert!(updated.body.sequence > initial.body.sequence);
        assert_eq!(updated.body.services, vec![service(2)]);
        updated
            .verify(&DiscoveryRecordValidationContext {
                current_unix_secs: current_unix_secs(),
                ..context()
            })
            .expect("updated record verifies");
    }

    #[test]
    fn derived_sequence_does_not_regress_after_restart() {
        let secret = secret_key();
        let (_connected_tx, connected_rx) = watch::channel(Vec::new());
        let first = discovery_handle_at(
            local_config_with(secret.clone(), vec![test_addr(44)], vec![service(1)]),
            ZakuraDiscoveryConfig::default(),
            connected_rx,
            NOW,
        );
        let (_connected_tx, connected_rx) = watch::channel(Vec::new());
        let restarted = discovery_handle_at(
            local_config_with(secret, vec![test_addr(44)], vec![service(1)]),
            ZakuraDiscoveryConfig::default(),
            connected_rx,
            NOW + 1,
        );

        assert!(
            restarted.current_self_record().body.sequence
                > first.current_self_record().body.sequence
        );
        assert_eq!(
            restarted.current_self_record().body.node_id,
            first.current_self_record().body.node_id
        );
    }

    #[test]
    fn persisted_sequence_seed_makes_fast_restart_record_newer() {
        let secret = secret_key();
        let same_unix_second = NOW;
        let same_sequence_tick = 10_000_000_000;
        let (_connected_tx, connected_rx) = watch::channel(Vec::new());
        let first = discovery_handle_at_with_sequence(
            local_config_with(secret.clone(), vec![test_addr(44)], vec![service(1)]),
            ZakuraDiscoveryConfig::default(),
            connected_rx,
            same_unix_second,
            same_sequence_tick,
        );
        let first_record = first.current_self_record();

        let mut restarted_config = local_config_with(secret, vec![test_addr(45)], vec![service(2)]);
        restarted_config.last_authored_sequence = Some(first_record.body.sequence);
        let (_connected_tx, connected_rx) = watch::channel(Vec::new());
        let restarted = discovery_handle_at_with_sequence(
            restarted_config,
            ZakuraDiscoveryConfig::default(),
            connected_rx,
            same_unix_second,
            same_sequence_tick,
        );
        let restarted_record = restarted.current_self_record();

        assert_eq!(restarted_record.body.node_id, first_record.body.node_id);
        assert!(restarted_record.body.sequence > first_record.body.sequence);
        assert_eq!(restarted_record.body.direct_addrs, vec![test_addr(45)]);
        assert_eq!(restarted_record.body.services, vec![service(2)]);
    }

    #[test]
    fn self_record_filters_non_routable_direct_addresses() {
        let (_connected_tx, connected_rx) = watch::channel(Vec::new());
        let handle = discovery_handle_at(
            local_config_with(
                secret_key(),
                vec![
                    SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 8233),
                    SocketAddr::new(IpAddr::V6(Ipv6Addr::LOCALHOST), 8233),
                    SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 8233),
                    SocketAddr::new(IpAddr::V6(Ipv6Addr::UNSPECIFIED), 8233),
                    test_addr(45),
                ],
                vec![service(1)],
            ),
            ZakuraDiscoveryConfig::default(),
            connected_rx,
            NOW,
        );

        assert_eq!(
            handle.current_self_record().body.direct_addrs,
            vec![test_addr(45)]
        );
    }

    #[tokio::test]
    async fn active_services_follow_supervisor_watch_connect_and_disconnect() {
        let (connected_tx, connected_rx) = watch::channel(Vec::new());
        let handle = discovery_handle_with_connected(connected_rx);
        let record = runtime_record_with(1, service(9), test_addr(9));
        let node_id = record.body.node_id;
        connected_tx.send_replace(vec![peer_id_for(node_id)]);
        handle
            .import_connected_peer_record(record, node_id)
            .await
            .expect("connected self-record imports");

        assert_eq!(
            handle.active_services(node_id).await,
            Some(vec![service(9)])
        );

        connected_tx.send_replace(Vec::new());
        assert_eq!(handle.active_services(node_id).await, None);
    }

    #[tokio::test]
    async fn active_services_drop_connected_hello_absent_from_supervisor_watch() {
        let (_connected_tx, connected_rx) = watch::channel(Vec::new());
        let handle = discovery_handle_with_connected(connected_rx);
        let record = runtime_record_with(1, service(9), test_addr(9));
        let node_id = record.body.node_id;
        handle
            .import_connected_peer_record(record, node_id)
            .await
            .expect("connected self-record imports");

        assert!(handle
            .service_candidates(&service(9), false, &[])
            .await
            .connected
            .is_empty());
        assert_eq!(handle.active_services(node_id).await, None);
        assert!(handle.inner.lock().await.active_services.is_empty());
    }

    #[tokio::test]
    async fn gossiped_service_record_does_not_create_active_services() {
        let (connected_tx, connected_rx) = watch::channel(Vec::new());
        let handle = discovery_handle_with_connected(connected_rx);
        let record = runtime_record_with(1, service(9), test_addr(9));
        let node_id = record.body.node_id;

        handle.import_peer_records([record], None).await;
        connected_tx.send_replace(vec![peer_id_for(node_id)]);

        assert_eq!(handle.active_services(node_id).await, None);
    }

    #[tokio::test]
    async fn connected_services_import_creates_and_updates_first_party_live_summary() {
        let (connected_tx, connected_rx) = watch::channel(Vec::new());
        let handle = discovery_handle_with_connected(connected_rx);
        let node_id = secret_key().public();
        let now = current_unix_secs();
        connected_tx.send_replace(vec![peer_id_for(node_id)]);

        let first_summary = DiscoveryServiceSummary {
            peer_exchange_slots_free: 1,
            ..discovery_summary()
        };
        handle
            .import_connected_peer_services_at(
                first_party_services(
                    node_id,
                    u64::MAX,
                    vec![ServiceSummaryEnvelope::discovery(&first_summary)
                        .expect("test discovery summary encodes")],
                ),
                node_id,
                now,
            )
            .await
            .expect("first-party services import");

        assert_eq!(
            handle.active_services(node_id).await,
            Some(vec![ZakuraServiceId::discovery()])
        );
        let cached = handle
            .live_service_summaries_at(node_id, now)
            .await
            .expect("live summary entry exists");
        assert_eq!(cached.len(), 1);
        assert_eq!(cached[0].observed_at_unix_secs, now);
        assert_eq!(
            cached[0].expires_at_unix_secs,
            now + DEFAULT_LIVE_SERVICE_SUMMARY_TTL.as_secs()
        );
        assert_eq!(
            cached[0].summary,
            ZakuraLiveServiceSummary::Discovery(first_summary)
        );

        let updated_summary = DiscoveryServiceSummary {
            peer_exchange_slots_free: 9,
            ..discovery_summary()
        };
        handle
            .import_connected_peer_services_at(
                first_party_services(
                    node_id,
                    now + 10,
                    vec![ServiceSummaryEnvelope::discovery(&updated_summary)
                        .expect("test discovery summary encodes")],
                ),
                node_id,
                now + 1,
            )
            .await
            .expect("newer first-party services update");

        let cached = handle
            .live_service_summaries_at(node_id, now + 1)
            .await
            .expect("updated live summary entry exists");
        assert_eq!(cached.len(), 1);
        assert_eq!(cached[0].observed_at_unix_secs, now + 1);
        assert_eq!(cached[0].expires_at_unix_secs, now + 10);
        assert_eq!(
            cached[0].summary,
            ZakuraLiveServiceSummary::Discovery(updated_summary)
        );
    }

    #[tokio::test]
    async fn connected_services_mismatched_node_id_rejected_without_live_summary() {
        let (connected_tx, connected_rx) = watch::channel(Vec::new());
        let handle = discovery_handle_with_connected(connected_rx);
        let peer_node_id = secret_key().public();
        let claimed_node_id = secret_key().public();
        connected_tx.send_replace(vec![peer_id_for(peer_node_id)]);

        let error = handle
            .import_connected_peer_services_at(
                first_party_services(
                    claimed_node_id,
                    NOW + 30,
                    vec![ServiceSummaryEnvelope::discovery(&discovery_summary())
                        .expect("test discovery summary encodes")],
                ),
                peer_node_id,
                NOW,
            )
            .await
            .expect_err("mismatched services are rejected");

        assert!(matches!(
            error,
            DiscoveryWireError::MismatchedNodeId {
                field: "services node id"
            }
        ));
        assert_eq!(
            handle.live_service_summaries_at(peer_node_id, NOW).await,
            None
        );
        assert_eq!(
            handle.live_service_summaries_at(claimed_node_id, NOW).await,
            None
        );
    }

    #[tokio::test]
    async fn peers_response_and_gossiped_record_do_not_create_live_summary() {
        let (connected_tx, connected_rx) = watch::channel(Vec::new());
        let handle = discovery_handle_with_connected(connected_rx);
        let record = runtime_record_with(1, service(9), test_addr(9));
        let node_id = record.body.node_id;
        let message = DiscoveryMessage::Peers {
            records: vec![record],
        };
        let encoded = message.encode().expect("test peers response encodes");
        let DiscoveryMessage::Peers { records } =
            DiscoveryMessage::decode(&encoded).expect("test peers response decodes")
        else {
            panic!("PEERS response decoded as a different message");
        };

        handle.import_peer_records(records, None).await;
        connected_tx.send_replace(vec![peer_id_for(node_id)]);

        assert_eq!(handle.live_service_summaries_at(node_id, NOW).await, None);
        assert_eq!(handle.active_services(node_id).await, None);
        assert!(handle.record_for(node_id).await.is_some());
    }

    #[tokio::test]
    async fn active_services_prefer_latest_connected_hello_over_book_record() {
        let (connected_tx, connected_rx) = watch::channel(Vec::new());
        let handle = discovery_handle_with_connected(connected_rx);
        let secret = secret_key();
        let active_record = runtime_record_with_secret(&secret, 1, service(1), test_addr(1));
        let stale_book_record = runtime_record_with_secret(&secret, 2, service(2), test_addr(2));
        let node_id = active_record.body.node_id;

        handle
            .import_connected_peer_record(active_record, node_id)
            .await
            .expect("connected self-record imports");
        handle
            .import_peer_record(stale_book_record, Some(secret_key().public()))
            .await
            .expect("newer third-party record imports into book");
        connected_tx.send_replace(vec![peer_id_for(node_id)]);

        assert_eq!(
            handle.active_services(node_id).await,
            Some(vec![service(1)])
        );
        assert_eq!(
            handle
                .service_candidates(&service(1), false, &[])
                .await
                .connected,
            vec![node_id]
        );
    }

    #[tokio::test]
    async fn active_services_ignore_stale_and_equal_sequence_conflicting_hellos() {
        let (connected_tx, connected_rx) = watch::channel(Vec::new());
        let handle = discovery_handle_with_connected(connected_rx);
        let secret = secret_key();
        let newer = runtime_record_with_secret(&secret, 2, service(2), test_addr(2));
        let older = runtime_record_with_secret(&secret, 1, service(1), test_addr(1));
        let equal_conflicting = runtime_record_with_secret(&secret, 2, service(3), test_addr(3));
        let stored_newer = runtime_record_with_secret(&secret, 4, service(4), test_addr(4));
        let older_than_stored = runtime_record_with_secret(&secret, 3, service(3), test_addr(3));
        let node_id = newer.body.node_id;
        connected_tx.send_replace(vec![peer_id_for(node_id)]);

        handle
            .import_connected_peer_record(newer, node_id)
            .await
            .expect("newer connected self-record imports");
        assert_eq!(
            handle
                .import_connected_peer_record(older, node_id)
                .await
                .expect("older connected self-record verifies and is ignored"),
            ImportOutcome::IgnoredOlder
        );
        assert_eq!(
            handle
                .import_connected_peer_record(equal_conflicting, node_id)
                .await
                .expect("equal-sequence connected self-record refreshes metadata only"),
            ImportOutcome::MetadataUpdated
        );
        handle
            .import_peer_record(stored_newer, Some(secret_key().public()))
            .await
            .expect("newer third-party record imports into book");
        assert_eq!(
            handle
                .import_connected_peer_record(older_than_stored, node_id)
                .await
                .expect("connected self-record older than the book is ignored"),
            ImportOutcome::IgnoredOlder
        );

        assert_eq!(
            handle.active_services(node_id).await,
            Some(vec![service(2)])
        );
        assert_eq!(
            handle
                .service_candidates(&service(2), false, &[])
                .await
                .connected,
            vec![node_id]
        );
        assert!(handle
            .service_candidates(&service(3), false, &[])
            .await
            .connected
            .is_empty());
    }

    #[tokio::test]
    async fn addressless_active_services_ignore_stale_and_equal_sequence_conflicting_hellos() {
        let (connected_tx, connected_rx) = watch::channel(Vec::new());
        let handle = discovery_handle_with_connected(connected_rx);
        let secret = secret_key();
        let newer = runtime_record_with_secret_and_addrs(&secret, 2, service(2), Vec::new());
        let older = runtime_record_with_secret_and_addrs(&secret, 1, service(1), Vec::new());
        let equal_conflicting =
            runtime_record_with_secret_and_addrs(&secret, 2, service(3), Vec::new());
        let node_id = newer.body.node_id;
        connected_tx.send_replace(vec![peer_id_for(node_id)]);

        assert!(matches!(
            handle.import_connected_peer_record(newer, node_id).await,
            Err(DiscoveryBookError::NoUsableDirectAddress)
        ));
        assert!(matches!(
            handle.import_connected_peer_record(older, node_id).await,
            Err(DiscoveryBookError::NoUsableDirectAddress)
        ));
        assert!(matches!(
            handle
                .import_connected_peer_record(equal_conflicting, node_id)
                .await,
            Err(DiscoveryBookError::NoUsableDirectAddress)
        ));

        assert_eq!(
            handle.active_services(node_id).await,
            Some(vec![service(2)])
        );
        assert_eq!(
            handle
                .service_candidates(&service(2), false, &[])
                .await
                .connected,
            vec![node_id]
        );
        assert!(handle
            .service_candidates(&service(3), false, &[])
            .await
            .connected
            .is_empty());
    }

    #[tokio::test]
    async fn expired_connected_hello_does_not_create_active_services() {
        let (connected_tx, connected_rx) = watch::channel(Vec::new());
        let handle = discovery_handle_with_connected(connected_rx);
        let secret = secret_key();
        let mut body = body(&secret);
        body.sequence = 1;
        body.direct_addrs = vec![test_addr(1)];
        body.services = vec![service(1)];
        body.expires_at_unix_secs = current_unix_secs()
            .saturating_sub(DEFAULT_DISCOVERY_CLOCK_SKEW_TOLERANCE.as_secs())
            .saturating_sub(1);
        let expired = ZakuraNodeRecord::sign(body, &secret).expect("expired record signs");
        let node_id = expired.body.node_id;
        connected_tx.send_replace(vec![peer_id_for(node_id)]);

        assert!(matches!(
            handle.import_connected_peer_record(expired, node_id).await,
            Err(DiscoveryBookError::Record(DiscoveryRecordError::Expired))
        ));
        assert_eq!(handle.active_services(node_id).await, None);
        assert!(handle
            .service_candidates(&service(1), false, &[])
            .await
            .connected
            .is_empty());
    }

    #[tokio::test]
    async fn service_candidates_require_explicit_general_fallback() {
        let (connected_tx, connected_rx) = watch::channel(Vec::new());
        let handle = discovery_handle_at(
            local_config_with(secret_key(), vec![test_addr(49)], vec![service(1)]),
            ZakuraDiscoveryConfig {
                max_zakura_connections: 8,
                discovery_connection_headroom: 1,
                ..ZakuraDiscoveryConfig::default()
            },
            connected_rx,
            NOW,
        );
        let active = runtime_record_with(1, service(1), test_addr(1));
        let active_id = active.body.node_id;
        let discovered = runtime_record_with(2, service(1), test_addr(2));
        let general = runtime_record_with(3, service(3), test_addr(3));
        handle
            .import_connected_peer_record(active, active_id)
            .await
            .expect("connected self-record imports");
        handle
            .import_peer_record(discovered.clone(), Some(discovered.body.node_id))
            .await
            .expect("discovered first-party record imports");
        handle
            .import_peer_record(general.clone(), Some(general.body.node_id))
            .await
            .expect("general first-party record imports");
        connected_tx.send_replace(vec![peer_id_for(active_id)]);

        let matching = handle.service_candidates(&service(1), true, &[]).await;
        assert_eq!(matching.connected, vec![active_id]);
        assert_eq!(matching.discovered, vec![candidate_for(&discovered, false)]);
        assert!(!matching.used_fallback);

        assert_eq!(
            handle
                .service_candidates(&service(2), false, &[])
                .await
                .discovered,
            Vec::<ZakuraDiscoveryDialCandidate>::new()
        );

        let fallback = handle
            .service_candidates(&service(2), true, &[discovered.body.node_id])
            .await;
        assert!(fallback.connected.is_empty());
        assert_eq!(fallback.discovered, vec![candidate_for(&general, false)]);
        assert!(fallback.used_fallback);
    }

    #[tokio::test]
    async fn live_first_party_summary_prioritizes_service_candidates_until_stale() {
        let (connected_tx, connected_rx) = watch::channel(Vec::new());
        let handle = discovery_handle_with_connected(connected_rx);
        let now = current_unix_secs();
        let first = runtime_record_with(1, ZakuraServiceId::header_sync(), test_addr(1));
        let second = runtime_record_with(2, ZakuraServiceId::header_sync(), test_addr(2));
        let first_id = first.body.node_id;
        let second_id = second.body.node_id;
        connected_tx.send_replace(vec![peer_id_for(first_id), peer_id_for(second_id)]);

        handle
            .import_connected_peer_record(first, first_id)
            .await
            .expect("first connected self-record imports");
        handle
            .import_connected_peer_record(second, second_id)
            .await
            .expect("second connected self-record imports");

        let initial = handle
            .service_candidates(&ZakuraServiceId::header_sync(), false, &[])
            .await
            .connected;
        assert_eq!(initial.len(), 2);
        let preferred_id = initial[1];
        let mut preferred_summary = header_summary();
        preferred_summary.inbound_slots_free = 8;
        preferred_summary.outbound_slots_free = 8;

        handle
            .import_connected_peer_services_at(
                first_party_services(
                    preferred_id,
                    now + 30,
                    vec![ServiceSummaryEnvelope::header_sync(&preferred_summary)
                        .expect("test header summary encodes")],
                ),
                preferred_id,
                now,
            )
            .await
            .expect("first-party header summary imports");

        let with_live_summary = handle
            .service_candidates(&ZakuraServiceId::header_sync(), false, &[])
            .await
            .connected;
        assert_eq!(with_live_summary[0], preferred_id);

        handle
            .import_connected_peer_services_at(
                first_party_services(
                    preferred_id,
                    now,
                    vec![ServiceSummaryEnvelope::header_sync(&preferred_summary)
                        .expect("test header summary encodes")],
                ),
                preferred_id,
                now,
            )
            .await
            .expect("expired first-party header summary is dropped");

        assert_eq!(
            handle
                .service_candidates(&ZakuraServiceId::header_sync(), false, &[])
                .await
                .connected,
            initial
        );
    }

    #[tokio::test]
    async fn header_sync_candidates_prefer_useful_first_party_height() {
        let (connected_tx, connected_rx) = watch::channel(Vec::new());
        let handle = discovery_handle_with_connected(connected_rx);
        let now = current_unix_secs();
        let without_summary = runtime_record_with(1, ZakuraServiceId::header_sync(), test_addr(1));
        let with_summary = runtime_record_with(2, ZakuraServiceId::header_sync(), test_addr(2));
        let without_summary_id = without_summary.body.node_id;
        let with_summary_id = with_summary.body.node_id;
        connected_tx.send_replace(vec![
            peer_id_for(without_summary_id),
            peer_id_for(with_summary_id),
        ]);

        handle
            .import_connected_peer_record(without_summary, without_summary_id)
            .await
            .expect("connected record imports");
        handle
            .import_connected_peer_record(with_summary, with_summary_id)
            .await
            .expect("connected record imports");

        let mut summary = header_summary();
        summary.best_height = block::Height(20);
        summary.inbound_slots_free = 1;
        handle
            .import_connected_peer_services_at(
                first_party_services(
                    with_summary_id,
                    now + 30,
                    vec![ServiceSummaryEnvelope::header_sync(&summary)
                        .expect("test header summary encodes")],
                ),
                with_summary_id,
                now,
            )
            .await
            .expect("first-party header summary imports");

        let candidates = handle
            .header_sync_candidates(
                &ZakuraHeaderSyncCandidateState {
                    target_height: block::Height(10),
                    ..ZakuraHeaderSyncCandidateState::default()
                },
                false,
                &[],
            )
            .await;

        assert_eq!(candidates.connected[0], with_summary_id);
        assert!(candidates.connected.contains(&without_summary_id));
    }

    #[tokio::test]
    async fn header_sync_candidates_skip_full_inbound_unless_already_admitted() {
        let (connected_tx, connected_rx) = watch::channel(Vec::new());
        let handle = discovery_handle_with_connected(connected_rx);
        let now = current_unix_secs();
        let full = runtime_record_with(1, ZakuraServiceId::header_sync(), test_addr(1));
        let open = runtime_record_with(2, ZakuraServiceId::header_sync(), test_addr(2));
        let full_id = full.body.node_id;
        let open_id = open.body.node_id;
        connected_tx.send_replace(vec![peer_id_for(full_id), peer_id_for(open_id)]);

        handle
            .import_connected_peer_record(full, full_id)
            .await
            .expect("connected record imports");
        handle
            .import_connected_peer_record(open, open_id)
            .await
            .expect("connected record imports");

        let mut full_summary = header_summary();
        full_summary.best_height = block::Height(20);
        full_summary.inbound_slots_free = 0;
        let mut open_summary = full_summary;
        open_summary.inbound_slots_free = 2;
        for (node_id, summary) in [(full_id, full_summary), (open_id, open_summary)] {
            handle
                .import_connected_peer_services_at(
                    first_party_services(
                        node_id,
                        now + 30,
                        vec![ServiceSummaryEnvelope::header_sync(&summary)
                            .expect("test header summary encodes")],
                    ),
                    node_id,
                    now,
                )
                .await
                .expect("first-party header summary imports");
        }

        let candidates = handle
            .header_sync_candidates(
                &ZakuraHeaderSyncCandidateState {
                    target_height: block::Height(10),
                    ..ZakuraHeaderSyncCandidateState::default()
                },
                false,
                &[],
            )
            .await;
        assert_eq!(candidates.connected, vec![open_id]);

        let candidates = handle
            .header_sync_candidates(
                &ZakuraHeaderSyncCandidateState {
                    target_height: block::Height(10),
                    admitted_node_ids: vec![full_id],
                    ..ZakuraHeaderSyncCandidateState::default()
                },
                false,
                &[],
            )
            .await;
        assert!(candidates.connected.contains(&full_id));
        assert!(candidates.connected.contains(&open_id));
    }

    #[tokio::test]
    async fn block_sync_service_candidates_prefer_covering_free_slot_summary() {
        let (connected_tx, connected_rx) = watch::channel(Vec::new());
        let handle = discovery_handle_with_connected(connected_rx);
        let now = current_unix_secs();
        let no_summary = runtime_record_with(1, ZakuraServiceId::block_sync(), test_addr(1));
        let covering = runtime_record_with(2, ZakuraServiceId::block_sync(), test_addr(2));
        let prefix = runtime_record_with(3, ZakuraServiceId::block_sync(), test_addr(3));
        let full = runtime_record_with(4, ZakuraServiceId::block_sync(), test_addr(4));
        let not_block_sync = runtime_record_with(5, ZakuraServiceId::header_sync(), test_addr(5));
        let no_summary_id = no_summary.body.node_id;
        let covering_id = covering.body.node_id;
        let prefix_id = prefix.body.node_id;
        let full_id = full.body.node_id;
        let not_block_sync_id = not_block_sync.body.node_id;
        connected_tx.send_replace(vec![
            peer_id_for(no_summary_id),
            peer_id_for(covering_id),
            peer_id_for(prefix_id),
            peer_id_for(full_id),
            peer_id_for(not_block_sync_id),
        ]);

        for record in [no_summary, covering, prefix, full, not_block_sync] {
            let node_id = record.body.node_id;
            handle
                .import_connected_peer_record(record, node_id)
                .await
                .expect("connected record imports");
        }

        let mut covering_summary = block_sync_summary();
        covering_summary.servable_low = block::Height(10);
        covering_summary.servable_high = block::Height(20);
        covering_summary.free_slots = 2;
        let mut prefix_summary = covering_summary;
        prefix_summary.servable_high = block::Height(12);
        prefix_summary.free_slots = 4;
        let mut full_summary = covering_summary;
        full_summary.free_slots = 0;
        for (node_id, summary) in [
            (covering_id, covering_summary),
            (prefix_id, prefix_summary),
            (full_id, full_summary),
        ] {
            handle
                .import_connected_peer_services_at(
                    first_party_services(
                        node_id,
                        now + 30,
                        vec![ServiceSummaryEnvelope::block_sync(&summary)
                            .expect("test block summary encodes")],
                    ),
                    node_id,
                    now,
                )
                .await
                .expect("first-party block summary imports");
        }

        let generic = handle
            .service_candidates(&ZakuraServiceId::block_sync(), false, &[])
            .await;
        assert!(generic.connected.contains(&covering_id));
        assert!(generic.connected.contains(&prefix_id));
        assert!(generic.connected.contains(&full_id));
        assert!(generic.connected.contains(&no_summary_id));
        assert!(!generic.connected.contains(&not_block_sync_id));

        let candidates = handle
            .block_sync_candidates(
                &ZakuraBlockSyncCandidateState {
                    missing_block_bodies: vec![block::Height(10), block::Height(20)],
                    ..ZakuraBlockSyncCandidateState::default()
                },
                false,
                &[],
            )
            .await;

        assert_eq!(candidates.connected[0], covering_id);
        let prefix_position = candidates
            .connected
            .iter()
            .position(|node_id| node_id == &prefix_id)
            .expect("prefix-covering peer remains a candidate");
        let no_summary_position = candidates
            .connected
            .iter()
            .position(|node_id| node_id == &no_summary_id)
            .expect("no-summary peer remains a candidate");
        assert!(prefix_position < no_summary_position);
        assert!(candidates.connected.contains(&no_summary_id));
        assert!(!candidates.connected.contains(&full_id));
        assert!(!candidates.connected.contains(&not_block_sync_id));

        let candidates = handle
            .block_sync_candidates(
                &ZakuraBlockSyncCandidateState {
                    missing_block_bodies: vec![block::Height(10), block::Height(20)],
                    admitted_node_ids: vec![full_id],
                },
                false,
                &[],
            )
            .await;

        assert!(candidates.connected.contains(&full_id));
    }

    #[tokio::test]
    async fn first_party_block_sync_summary_only_imports_from_authenticated_peer() {
        let (connected_tx, connected_rx) = watch::channel(Vec::new());
        let handle = discovery_handle_with_connected(connected_rx);
        let peer_node_id = secret_key().public();
        let claimed_node_id = secret_key().public();
        let now = current_unix_secs();
        connected_tx.send_replace(vec![peer_id_for(peer_node_id)]);

        let error = handle
            .import_connected_peer_services_at(
                first_party_services(
                    claimed_node_id,
                    now + 30,
                    vec![ServiceSummaryEnvelope::block_sync(&block_sync_summary())
                        .expect("test block summary encodes")],
                ),
                peer_node_id,
                now,
            )
            .await
            .expect_err("mismatched first-party block summary is rejected");

        assert!(matches!(
            error,
            DiscoveryWireError::MismatchedNodeId {
                field: "services node id"
            }
        ));
        assert_eq!(
            handle.live_service_summaries_at(peer_node_id, now).await,
            None
        );
        assert_eq!(
            handle.live_service_summaries_at(claimed_node_id, now).await,
            None
        );

        let record = runtime_record_with(10, ZakuraServiceId::block_sync(), test_addr(10));
        let node_id = record.body.node_id;
        let message = DiscoveryMessage::Peers {
            records: vec![record],
        };
        let encoded = message.encode().expect("test peers response encodes");
        let DiscoveryMessage::Peers { records } =
            DiscoveryMessage::decode(&encoded).expect("test peers response decodes")
        else {
            panic!("PEERS response decoded as a different message");
        };

        handle.import_peer_records(records, None).await;
        connected_tx.send_replace(vec![peer_id_for(peer_node_id), peer_id_for(node_id)]);

        assert_eq!(handle.live_service_summaries_at(node_id, now).await, None);
        assert_eq!(handle.active_services(node_id).await, None);
    }

    #[tokio::test]
    async fn block_sync_summary_mismatch_changes_selection_without_punitive_state() {
        let (connected_tx, connected_rx) = watch::channel(Vec::new());
        let handle = discovery_handle_with_connected(connected_rx);
        let now = current_unix_secs();
        let peer = runtime_record_with(1, ZakuraServiceId::block_sync(), test_addr(1));
        let covering_peer = runtime_record_with(2, ZakuraServiceId::block_sync(), test_addr(2));
        let peer_id = peer.body.node_id;
        let covering_peer_id = covering_peer.body.node_id;
        connected_tx.send_replace(vec![peer_id_for(peer_id), peer_id_for(covering_peer_id)]);
        for record in [peer, covering_peer] {
            let node_id = record.body.node_id;
            handle
                .import_connected_peer_record(record, node_id)
                .await
                .expect("connected record imports");
        }

        let mut summary = block_sync_summary();
        summary.servable_low = block::Height(10);
        summary.servable_high = block::Height(20);
        summary.free_slots = 3;
        let mut covering_summary = summary;
        covering_summary.free_slots = 1;
        for (node_id, summary) in [(peer_id, summary), (covering_peer_id, covering_summary)] {
            handle
                .import_connected_peer_services_at(
                    first_party_services(
                        node_id,
                        now + 30,
                        vec![ServiceSummaryEnvelope::block_sync(&summary)
                            .expect("test block summary encodes")],
                    ),
                    node_id,
                    now,
                )
                .await
                .expect("first-party block summary imports");
        }

        let state = ZakuraBlockSyncCandidateState {
            missing_block_bodies: vec![block::Height(10), block::Height(20)],
            ..ZakuraBlockSyncCandidateState::default()
        };
        assert_eq!(
            handle
                .block_sync_candidates(&state, false, &[])
                .await
                .connected
                .first(),
            Some(&peer_id)
        );

        summary.servable_high = block::Height(11);
        handle
            .import_connected_peer_services_at(
                first_party_services(
                    peer_id,
                    now + 31,
                    vec![ServiceSummaryEnvelope::block_sync(&summary)
                        .expect("test block summary encodes")],
                ),
                peer_id,
                now + 1,
            )
            .await
            .expect("corrected first-party block summary imports");

        let candidates = handle.block_sync_candidates(&state, false, &[]).await;
        assert_eq!(candidates.connected.first(), Some(&covering_peer_id));
        assert!(candidates.connected.contains(&peer_id));
        let cached = handle
            .live_service_summaries_at(peer_id, now + 1)
            .await
            .expect("live summary remains cached");
        assert_eq!(
            cached[0].summary,
            ZakuraLiveServiceSummary::BlockSync(summary)
        );
    }

    #[tokio::test]
    async fn live_summaries_expire_independently_of_signed_records() {
        let (connected_tx, connected_rx) = watch::channel(Vec::new());
        let handle = discovery_handle_with_connected(connected_rx);
        let record = runtime_record_with(1, ZakuraServiceId::discovery(), test_addr(1));
        let node_id = record.body.node_id;
        connected_tx.send_replace(vec![peer_id_for(node_id)]);

        handle
            .import_connected_peer_record(record.clone(), node_id)
            .await
            .expect("connected self-record imports");
        handle
            .import_connected_peer_services_at(
                first_party_services(
                    node_id,
                    NOW + 5,
                    vec![ServiceSummaryEnvelope::discovery(&discovery_summary())
                        .expect("test discovery summary encodes")],
                ),
                node_id,
                NOW,
            )
            .await
            .expect("first-party discovery summary imports");

        assert_eq!(
            handle
                .live_service_summaries_at(node_id, NOW)
                .await
                .expect("fresh summary is cached")
                .len(),
            1
        );
        assert_eq!(
            handle.live_service_summaries_at(node_id, NOW + 6).await,
            Some(Vec::new())
        );
        assert_eq!(handle.record_for(node_id).await, Some(record));
        assert_eq!(
            handle.active_services(node_id).await,
            Some(vec![ZakuraServiceId::discovery()])
        );
    }

    #[tokio::test]
    async fn expired_live_only_service_membership_is_pruned() {
        let (connected_tx, connected_rx) = watch::channel(Vec::new());
        let handle = discovery_handle_with_connected(connected_rx);
        let service = service(42);
        let live_only_node_id = secret_key().public();
        let record_backed = runtime_record_with(1, service.clone(), test_addr(42));
        let record_backed_node_id = record_backed.body.node_id;
        let now = current_unix_secs();
        connected_tx.send_replace(vec![
            peer_id_for(live_only_node_id),
            peer_id_for(record_backed_node_id),
        ]);

        handle
            .import_connected_peer_record(record_backed, record_backed_node_id)
            .await
            .expect("connected record imports");
        for node_id in [live_only_node_id, record_backed_node_id] {
            handle
                .import_connected_peer_services_at(
                    first_party_services(
                        node_id,
                        now + 30,
                        vec![ServiceSummaryEnvelope {
                            service_id: service.clone(),
                            summary_tag: 999,
                            summary_bytes: vec![1, 2, 3],
                        }],
                    ),
                    node_id,
                    now,
                )
                .await
                .expect("first-party live summary imports");
        }

        assert_eq!(
            handle.active_services(live_only_node_id).await,
            Some(vec![service.clone()])
        );
        assert_eq!(
            handle.active_services(record_backed_node_id).await,
            Some(vec![service.clone()])
        );
        let fresh_candidates = handle
            .service_candidates(&service, false, &[])
            .await
            .connected;
        assert!(fresh_candidates.contains(&live_only_node_id));
        assert!(fresh_candidates.contains(&record_backed_node_id));

        for node_id in [live_only_node_id, record_backed_node_id] {
            handle
                .import_connected_peer_services_at(
                    first_party_services(
                        node_id,
                        now,
                        vec![ServiceSummaryEnvelope {
                            service_id: service.clone(),
                            summary_tag: 999,
                            summary_bytes: vec![1, 2, 3],
                        }],
                    ),
                    node_id,
                    now,
                )
                .await
                .expect("expired first-party live summary is dropped");
        }

        assert_eq!(handle.active_services(live_only_node_id).await, None);
        assert_eq!(
            handle.active_services(record_backed_node_id).await,
            Some(vec![service.clone()])
        );
        let stale_candidates = handle
            .service_candidates(&service, false, &[])
            .await
            .connected;
        assert!(!stale_candidates.contains(&live_only_node_id));
        assert!(stale_candidates.contains(&record_backed_node_id));
    }

    #[tokio::test]
    async fn handle_imports_records_through_storage_validation() {
        let (_connected_tx, connected_rx) = watch::channel(Vec::new());
        let handle = discovery_handle_with_connected(connected_rx);
        let valid = runtime_record_with(1, service(1), test_addr(1));
        let signing_secret = secret_key();
        let mut no_addr = body(&signing_secret);
        no_addr.direct_addrs = Vec::new();
        no_addr.expires_at_unix_secs = current_unix_secs() + DEFAULT_DISCOVERY_RECORD_TTL.as_secs();
        let invalid = ZakuraNodeRecord::sign(no_addr, &signing_secret).expect("record signs");

        let outcome = handle.import_peer_records([valid, invalid], None).await;

        assert_eq!(outcome.attempted, 2);
        assert_eq!(outcome.added, 1);
        assert_eq!(outcome.rejected, 1);
    }

    /// Regression test for `claude-discovery-expensive-work-under-global-mutex`.
    ///
    /// An authenticated discovery peer can send a full `Peers` batch whose records carry
    /// invalid signatures. Each record still triggers a full Ed25519 verification — the
    /// expensive, attacker-driven work. Previously that verification ran while holding the
    /// global discovery mutex, so an attacker could repeatedly stall every other discovery
    /// operation (admission, sampling, dial selection). Signatures are now verified outside
    /// the lock, and an all-invalid batch never needs the lock at all, so the import makes
    /// progress even while another task holds the mutex.
    #[tokio::test]
    async fn import_peer_records_verifies_signatures_without_holding_global_mutex() {
        let (_connected_tx, connected_rx) = watch::channel(Vec::new());
        let handle = discovery_handle_with_connected(connected_rx);

        // A full Peers batch of records with valid bodies but tampered signatures: bumping
        // the sequence after signing leaves body validation passing while the Ed25519 check
        // (re-encoding the body and verifying) runs in full and fails.
        let mut batch = Vec::with_capacity(MAX_DISCOVERY_RECORDS_PER_RESPONSE);
        for index in 0..MAX_DISCOVERY_RECORDS_PER_RESPONSE {
            let octet = u8::try_from(index + 1).expect("test batch index fits in u8");
            let sequence = u64::try_from(index + 1).expect("test batch index fits in u64");
            let mut record = runtime_record_with(sequence, service(1), test_addr(octet));
            record.body.sequence += 1;
            batch.push(record);
        }

        // Hold the global discovery mutex for the entire import call.
        let guard = handle.inner.lock().await;

        // Verification and rejection of the whole batch must make progress without the lock;
        // if it were still performed under the mutex this would deadlock until the timeout.
        let outcome = tokio::time::timeout(
            Duration::from_secs(5),
            handle.import_peer_records(batch, None),
        )
        .await
        .expect("import_peer_records must verify signatures without holding the global mutex");

        drop(guard);

        assert_eq!(outcome.attempted, MAX_DISCOVERY_RECORDS_PER_RESPONSE);
        assert_eq!(outcome.rejected, MAX_DISCOVERY_RECORDS_PER_RESPONSE);
        assert_eq!(outcome.added, 0);
    }

    /// Regression test for `claude-discovery-expensive-work-under-global-mutex` (GetPeers sampling
    /// facet).
    ///
    /// `sample_peers` runs under the global discovery mutex and is driven by attacker-paced GetPeers
    /// requests. It previously cloned *every* record that passed the filter — up to the whole book
    /// (`max_records`, default 10_000) — even though at most `max_imported_records_per_response`
    /// records are ever returned. The clone is now bounded to the chosen sample.
    ///
    /// The bound is proven machine-independently by comparing the production `sample_peers` against
    /// an in-test reference that reproduces the pre-fix clone-the-whole-filtered-set behavior, over
    /// the same large book in the same run. With the fix, production clones only the returned sample
    /// and is markedly cheaper than the clone-everything reference; before the fix the two are the
    /// same computation, so the production-is-cheaper bound fails.
    #[test]
    fn sample_peers_clone_cost_is_bounded_by_returned_sample() {
        const BOOK: usize = 1024;
        const ITERS: usize = 400;

        let wanted = service(1);
        // Heavy records (maximum direct-address fan-out plus many services, with the wanted service
        // first so the filter match itself stays cheap) make each *record clone* far more expensive
        // than the allocation-free per-entry filter scan, so the clone count is the dominant cost
        // and the bounded-vs-unbounded clone gap is large.
        let addrs: Vec<SocketAddr> = (1u8..=MAX_DIRECT_ADDRS_PER_RECORD as u8)
            .map(test_addr)
            .collect();
        let mut services = vec![wanted.clone()];
        services.extend((100..100 + (MAX_SERVICES_PER_RECORD - 1)).map(service));

        let mut book = ZakuraDiscoveryBook::new(ZakuraDiscoveryBookLimits {
            max_records: BOOK,
            ..ZakuraDiscoveryBookLimits::default()
        });
        for seq in 0..BOOK {
            let secret = secret_key();
            let mut record_body = body(&secret);
            record_body.sequence = seq as u64 + 1;
            record_body.direct_addrs = addrs.clone();
            record_body.services = services.clone();
            let record = ZakuraNodeRecord::sign(record_body, &secret).expect("test record signs");
            book.import_record(record, None, NOW, &context())
                .expect("test record imports");
        }

        let cap = book.limits.max_imported_records_per_response;

        // Reference implementation: the pre-fix behavior of cloning every record that passes the
        // filter, then reservoir-sampling the clones. Mirrors `sample_peers`'s filter exactly.
        let naive_sample =
            |book: &ZakuraDiscoveryBook, rng: &mut StdRng| -> Vec<ZakuraNodeRecord> {
                book.entries
                    .iter()
                    .filter(|(node_id, entry)| {
                        book.local_node_id != Some(**node_id)
                            && !entry_is_expired(entry, NOW)
                            && has_wanted_services(&entry.record, std::slice::from_ref(&wanted))
                            && has_discovery_dialable_direct_addrs(&entry.record)
                    })
                    .map(|(_, entry)| entry.record.clone())
                    .choose_multiple(rng, cap)
            };

        let mut rng = StdRng::seed_from_u64(5);

        // Warm up the allocator and instruction caches before timing.
        let _ = naive_sample(&book, &mut rng);
        let _ = book.sample_peers(cap, std::slice::from_ref(&wanted), &[], NOW, &mut rng);

        let naive_start = std::time::Instant::now();
        for _ in 0..ITERS {
            assert_eq!(naive_sample(&book, &mut rng).len(), cap);
        }
        let naive_time = naive_start.elapsed();

        let production_start = std::time::Instant::now();
        for _ in 0..ITERS {
            assert_eq!(
                book.sample_peers(cap, std::slice::from_ref(&wanted), &[], NOW, &mut rng)
                    .len(),
                cap
            );
        }
        let production_time = production_start.elapsed();

        // Both run the same O(book) filter scan and reservoir sampling; the only difference is how
        // many records are cloned (production: the returned `cap`; reference: every match in the
        // book). With the bound in place production must be clearly cheaper than cloning the whole
        // filtered set. Before the fix they are the identical computation, so production is not
        // meaningfully cheaper and this fails. The 0.8 factor is a ratio on the same machine, so it
        // is machine-independent.
        assert!(
            production_time.as_nanos() * 5 < naive_time.as_nanos() * 4,
            "sample_peers did not bound its clone work: production={production_time:?} \
             clone-everything reference={naive_time:?}; production should be clearly cheaper than \
             cloning every filtered record"
        );
    }

    /// Guards the bounded top-k dial-candidate selection added for
    /// `claude-discovery-expensive-work-under-global-mutex`.
    ///
    /// `dial_candidates` now selects the best `limit` candidates with a partial select instead of
    /// sorting the whole book before `take(limit)`. With far more matching candidates than `limit`,
    /// the result must still be exactly the highest dial-priority candidates (most recent
    /// successful dial first), proving the partial selection keeps the same ordering as a full
    /// sort.
    #[test]
    fn dial_candidates_selects_top_priority_subset_under_limit() {
        let mut book = ZakuraDiscoveryBook::default();
        let records = (1u8..=10)
            .map(|index| signed_record_with(index.into(), service(1), test_addr(index)))
            .collect::<Vec<_>>();
        for record in &records {
            import_confirmed_record(&mut book, record.clone()).expect("test record imports");
        }

        // Three candidates get strictly increasing last-success times, so dial priority is
        // deterministic (most recent success first); the remaining seven have no successful dial.
        book.mark_dial_success(&records[5].body.node_id, NOW + 1);
        book.mark_dial_success(&records[3].body.node_id, NOW + 2);
        book.mark_dial_success(&records[7].body.node_id, NOW + 3);
        let expected_top = vec![
            records[7].body.node_id,
            records[3].body.node_id,
            records[5].body.node_id,
        ];

        let mut rng = StdRng::seed_from_u64(99);
        let selected = book.dial_candidates(
            3,
            &[service(1)],
            DialCandidateExclusions {
                connected_node_ids: &[],
                in_flight_node_ids: &[],
            },
            NOW,
            (
                DEFAULT_DISCOVERY_DIAL_BACKOFF_BASE,
                DEFAULT_DISCOVERY_DIAL_BACKOFF_MAX,
            ),
            &mut rng,
        );

        assert_eq!(
            selected
                .iter()
                .map(|candidate| candidate.node_id)
                .collect::<Vec<_>>(),
            expected_top,
        );
    }

    #[tokio::test]
    async fn handle_samples_records_through_storage_path() {
        let (_connected_tx, connected_rx) = watch::channel(Vec::new());
        let handle = discovery_handle_with_connected(connected_rx);
        let wanted = service(1);
        let matching = runtime_record_with(1, wanted.clone(), test_addr(1));
        let excluded = runtime_record_with(2, wanted.clone(), test_addr(2));
        let excluded_id = excluded.body.node_id;
        let other = runtime_record_with(3, service(2), test_addr(3));
        handle
            .import_peer_records([matching.clone(), excluded, other], None)
            .await;

        let sample = handle
            .sample_peers(10, std::slice::from_ref(&wanted), &[excluded_id])
            .await;

        assert_eq!(sample, vec![matching]);
    }

    #[tokio::test]
    async fn discovery_state_construction_works_with_default_config() {
        let (_connected_tx, connected_rx) = watch::channel(Vec::new());
        let handle = ZakuraDiscoveryHandle::new(
            local_config_with(secret_key(), vec![test_addr(46)], vec![service(1)]),
            ZakuraDiscoveryConfig::default(),
            connected_rx,
        )
        .expect("default discovery state constructs");

        assert_eq!(
            handle.current_self_record().body.direct_addrs,
            vec![test_addr(46)]
        );
    }

    #[tokio::test]
    async fn dial_candidates_exclude_connected_peers_and_respect_soft_cap() {
        let (connected_tx, connected_rx) = watch::channel(Vec::new());
        let handle = discovery_handle_at(
            local_config_with(secret_key(), vec![test_addr(47)], vec![service(1)]),
            ZakuraDiscoveryConfig {
                max_zakura_connections: 2,
                discovery_connection_headroom: 1,
                ..ZakuraDiscoveryConfig::default()
            },
            connected_rx,
            NOW,
        );
        let candidate = runtime_record_with(1, service(1), test_addr(1));
        let connected = runtime_record_with(2, service(1), test_addr(2));
        let connected_id = connected.body.node_id;
        let candidate_id = candidate.body.node_id;
        handle
            .import_peer_record(candidate.clone(), Some(candidate_id))
            .await
            .expect("candidate first-party record imports");
        handle
            .import_connected_peer_record(connected, connected_id)
            .await
            .expect("connected self-record imports");

        connected_tx.send_replace(vec![peer_id_for(connected_id)]);
        assert!(handle.dial_candidates(&[service(1)], &[]).await.is_empty());

        let (connected_tx, connected_rx) = watch::channel(Vec::new());
        let handle = discovery_handle_at(
            local_config_with(secret_key(), vec![test_addr(48)], vec![service(1)]),
            ZakuraDiscoveryConfig {
                max_zakura_connections: 4,
                discovery_connection_headroom: 1,
                ..ZakuraDiscoveryConfig::default()
            },
            connected_rx,
            NOW,
        );
        let connected = runtime_record_with(2, service(1), test_addr(2));
        let connected_id = connected.body.node_id;
        let candidate_id = candidate.body.node_id;
        handle
            .import_peer_record(candidate.clone(), Some(candidate_id))
            .await
            .expect("candidate first-party record imports");
        handle
            .import_connected_peer_record(connected, connected_id)
            .await
            .expect("connected self-record imports");

        connected_tx.send_replace(vec![peer_id_for(connected_id)]);
        assert_eq!(
            handle.dial_candidates(&[service(1)], &[]).await,
            vec![candidate_for(&candidate, false)]
        );
    }

    #[tokio::test]
    async fn dial_candidates_exclude_in_flight_peers() {
        let (_connected_tx, connected_rx) = watch::channel(Vec::new());
        let handle = discovery_handle_with_connected(connected_rx);
        let candidate = runtime_record_with(1, service(1), test_addr(1));
        let candidate_id = candidate.body.node_id;
        handle
            .import_peer_record(candidate, Some(secret_key().public()))
            .await
            .expect("candidate imports");

        assert!(handle
            .dial_candidates(&[service(1)], &[candidate_id])
            .await
            .is_empty());
    }

    #[test]
    fn discovery_dial_slot_limit_reserves_headroom_and_concurrent_cap() {
        assert_eq!(discovery_dial_slot_limit(0, 0, 8, 2, 4), 4);
        assert_eq!(discovery_dial_slot_limit(5, 0, 8, 2, 4), 1);
        assert_eq!(discovery_dial_slot_limit(5, 1, 8, 2, 4), 1);
        assert_eq!(discovery_dial_slot_limit(2, 4, 8, 2, 4), 0);
        assert_eq!(discovery_dial_slot_limit(2, 0, 2, 4, 4), 0);
    }

    #[tokio::test]
    async fn handle_records_are_owned_after_storage_changes() {
        let (_connected_tx, connected_rx) = watch::channel(Vec::new());
        let handle = discovery_handle_with_connected(connected_rx);
        let secret = secret_key();
        let old = {
            let mut record_body = body(&secret);
            record_body.sequence = 1;
            record_body.direct_addrs = vec![test_addr(10)];
            record_body.services = vec![service(1)];
            record_body.expires_at_unix_secs =
                current_unix_secs() + DEFAULT_DISCOVERY_RECORD_TTL.as_secs();
            ZakuraNodeRecord::sign(record_body, &secret).expect("old record signs")
        };
        let new = {
            let mut record_body = body(&secret);
            record_body.sequence = 2;
            record_body.direct_addrs = vec![test_addr(11)];
            record_body.services = vec![service(1)];
            record_body.expires_at_unix_secs =
                current_unix_secs() + DEFAULT_DISCOVERY_RECORD_TTL.as_secs();
            ZakuraNodeRecord::sign(record_body, &secret).expect("new record signs")
        };

        handle.import_peer_records([old.clone()], None).await;
        let sample = handle.sample_peers(1, &[service(1)], &[]).await;
        handle.import_peer_records([new], None).await;

        assert_eq!(sample, vec![old]);
    }
}
