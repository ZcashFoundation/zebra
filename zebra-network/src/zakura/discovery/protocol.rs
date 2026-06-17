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
use zebra_chain::primitives::ed25519::{Signature, SigningKey, VerificationKey};

use crate::zakura::{ZakuraNetworkId, ZakuraPeerId};

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
/// Reserved service discovery request.
pub const MSG_DISCOVERY_GET_SERVICES: u8 = 4;
/// Reserved service discovery response.
pub const MSG_DISCOVERY_SERVICES: u8 = 5;

/// Maximum bytes in an encoded discovery message.
pub const MAX_DISCOVERY_MESSAGE_BYTES: usize = 16 * 1024;
/// Maximum bytes in an encoded node record body.
pub const MAX_NODE_RECORD_BODY_BYTES: usize = 16 * 1024;
/// Maximum direct addresses in a node record.
pub const MAX_DIRECT_ADDRS_PER_RECORD: usize = 8;
/// Maximum services in a node record or query.
pub const MAX_SERVICES_PER_RECORD: usize = 32;
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

/// Native peer discovery service id.
pub const SERVICE_ID_DISCOVERY: &str = "zakura.discovery.v1";
/// Native legacy gossip service id.
pub const SERVICE_ID_LEGACY_GOSSIP: &str = "zakura.legacy_gossip.v1";
/// Native legacy requests service id.
pub const SERVICE_ID_LEGACY_REQUESTS: &str = "zakura.legacy_requests.v1";
/// Native service discovery service id.
pub const SERVICE_ID_SERVICE_DISCOVERY: &str = "zakura.service_discovery.v1";

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
    /// Reserved service query.
    GetServices {
        /// Service filter.
        wanted_services: Vec<ZakuraServiceId>,
        /// Maximum requested records.
        limit: u16,
        /// Node ids the responder should not return.
        exclude_node_ids: Vec<NodeId>,
    },
    /// Reserved service response using signed records.
    Services {
        /// Signed node records.
        records: Vec<ZakuraNodeRecord>,
    },
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
            Self::GetServices {
                wanted_services,
                limit,
                exclude_node_ids,
            } => {
                validate_query_fields(*limit, wanted_services, exclude_node_ids)?;
                bytes.write_u8(MSG_DISCOVERY_GET_SERVICES)?;
                encode_query_fields(*limit, wanted_services, exclude_node_ids, &mut bytes)?;
            }
            Self::Services { records } => {
                encode_records_message(
                    MSG_DISCOVERY_SERVICES,
                    records,
                    MAX_DISCOVERY_RECORDS_PER_RESPONSE,
                    &mut bytes,
                )?;
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
            MSG_DISCOVERY_GET_SERVICES => {
                let (limit, wanted_services, exclude_node_ids) = decode_query_fields(&mut reader)?;
                Self::GetServices {
                    wanted_services,
                    limit,
                    exclude_node_ids,
                }
            }
            MSG_DISCOVERY_SERVICES => Self::Services {
                records: decode_record_list(&mut reader, MAX_DISCOVERY_RECORDS_PER_RESPONSE)?,
            },
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

    /// A decoded payload had trailing bytes.
    #[error("trailing bytes in Zakura discovery payload")]
    TrailingBytes,

    /// A required field was empty.
    #[error("empty Zakura discovery {0}")]
    Empty(&'static str),

    /// A service id was not ASCII.
    #[error("Zakura discovery service id is not ASCII")]
    NonAsciiServiceId,

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
/// Candidates may come from signed discovery records or from trusted static bootstrap
/// configuration. Only signed records are eligible for peer samples; unsigned static candidates are
/// local dial hints.
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
    last_connected_node_ids: HashSet<NodeId>,
    local: ZakuraLocalDiscoveryState,
    config: ZakuraDiscoveryConfig,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ZakuraActiveServiceEntry {
    record: ZakuraNodeRecord,
    services: Vec<ZakuraServiceId>,
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
        let book = ZakuraDiscoveryBook::with_local_node_id(config.book_limits, local.node_id);
        Ok(Self {
            inner: Arc::new(Mutex::new(ZakuraDiscoveryInner {
                book,
                active_services: HashMap::new(),
                last_connected_node_ids: HashSet::new(),
                local,
                config,
            })),
            connected,
            self_record: self_record_rx,
            self_record_tx,
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
    pub async fn import_peer_records(
        &self,
        records: impl IntoIterator<Item = ZakuraNodeRecord>,
        source: Option<NodeId>,
    ) -> ImportBatchOutcome {
        let now = current_unix_secs();
        let mut inner = self.inner.lock().await;
        let context = inner.validation_context(now);
        inner.book.import_records(records, source, now, &context)
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
        inner.sync_active_services(&connected_node_ids);

        let mut connected: Vec<_> = inner
            .active_services
            .iter()
            .filter(|(node_id, entry)| {
                connected_node_ids.contains(node_id) && entry.services.contains(service)
            })
            .map(|(node_id, _)| *node_id)
            .collect();
        connected.sort_by_key(node_id_sort_key);

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

    /// Returns a connected peer's advertised services as a derived supervisor-watch projection.
    pub async fn active_services(&self, node_id: NodeId) -> Option<Vec<ZakuraServiceId>> {
        let connected = connected_peer_node_ids(&self.connected.borrow());
        let mut inner = self.inner.lock().await;
        inner.sync_active_services(&connected);
        if !connected.contains(&node_id) {
            return None;
        }

        inner
            .active_services
            .get(&node_id)
            .map(|entry| entry.services.clone())
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

    fn sync_active_services(&mut self, connected_node_ids: &[NodeId]) {
        let connected_node_ids: HashSet<_> = connected_node_ids.iter().copied().collect();
        self.active_services
            .retain(|node_id, _| connected_node_ids.contains(node_id));
        self.last_connected_node_ids = connected_node_ids;
    }

    fn update_active_services_from_connected_record(
        &mut self,
        node_id: NodeId,
        record: ZakuraNodeRecord,
        services: Vec<ZakuraServiceId>,
        import_result: &Result<ImportOutcome, DiscoveryBookError>,
    ) {
        if self
            .active_services
            .get(&node_id)
            .is_some_and(|active| record.body.sequence <= active.record.body.sequence)
        {
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
                .insert(node_id, ZakuraActiveServiceEntry { record, services });
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
        self.import_record_inner(record, source, false, now, context)
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
        self.import_record_inner(record, None, true, now, context)
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

        self.entries
            .iter()
            .filter(|(node_id, entry)| {
                !exclude_node_ids.contains(*node_id)
                    && self.local_node_id != Some(**node_id)
                    && !entry_is_expired(entry, now)
                    && has_wanted_services(&entry.record, wanted_services)
                    && has_discovery_dialable_direct_addrs(&entry.record)
            })
            .map(|(_, entry)| entry.record.clone())
            .choose_multiple(rng, limit)
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
        let mut candidates: Vec<_> = self
            .entries
            .values()
            .filter(|entry| {
                !connected_node_ids.contains(&entry.record.body.node_id)
                    && !in_flight_node_ids.contains(&entry.record.body.node_id)
                    && self.local_node_id != Some(entry.record.body.node_id)
                    && !entry_is_expired(entry, now)
                    && !entry_in_dial_backoff(entry, now, dial_backoff.0, dial_backoff.1)
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
                {
                    return None;
                }
                Some(DialCandidateRef::StaticConfigured(candidate))
            }))
            .collect();

        candidates.sort_by_cached_key(|entry| {
            let non_static_random_tie = if !entry.is_static() {
                rng.gen::<u64>()
            } else {
                0
            };
            let static_deterministic_tie = if entry.is_static() {
                node_id_sort_key(&entry.node_id())
            } else {
                [0; NODE_ID_BYTES]
            };
            (
                !entry.is_static(),
                Reverse(entry.last_success().unwrap_or(0)),
                entry.failure_count(),
                Reverse(entry.last_seen()),
                non_static_random_tie,
                static_deterministic_tie,
            )
        });

        candidates
            .into_iter()
            .take(limit)
            .map(DialCandidateRef::into_candidate)
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
        self.import_validated_record(entry.record, metadata, now, context)
    }

    fn import_record_inner(
        &mut self,
        record: ZakuraNodeRecord,
        source: Option<NodeId>,
        is_static: bool,
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
        self.import_validated_record(record, metadata, now, context)
    }

    fn import_validated_record(
        &mut self,
        record: ZakuraNodeRecord,
        metadata: DiscoveryEntryMetadata,
        now: u64,
        context: &DiscoveryRecordValidationContext,
    ) -> Result<ImportOutcome, DiscoveryBookError> {
        if self.local_node_id == Some(record.body.node_id) {
            return Err(DiscoveryBookError::SelfRecord);
        }

        record.verify(context)?;
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

    entry.source = metadata.source;
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
/// Untrusted peer/gossip records must only contain globally dialable addresses, preserving the
/// discovery security rule that gossiped records cannot inject loopback, link-local, multicast, or
/// broadcast targets. Static records are trusted-by-configuration bootstrap records, so they may use
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
        }
        IpAddr::V6(ip) => {
            !ip.is_unspecified()
                && !ip.is_loopback()
                && !ip.is_multicast()
                && !is_ipv6_unicast_link_local(&ip)
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
    if services.len() > MAX_SERVICES_PER_RECORD {
        return Err(DiscoveryWireError::OversizedPayload {
            actual: services.len(),
            max: MAX_SERVICES_PER_RECORD,
        });
    }
    writer.write_u16::<LittleEndian>(u16_from_usize(services.len(), "service count")?)?;
    for service in services {
        let bytes = service.as_str().as_bytes();
        validate_service_id(bytes)?;
        writer.write_u16::<LittleEndian>(u16_from_usize(bytes.len(), "service id length")?)?;
        writer.write_all(bytes)?;
    }
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
        let len = usize::from(reader.read_u16::<LittleEndian>()?);
        if len > MAX_ZAKURA_SERVICE_ID_BYTES {
            return Err(DiscoveryWireError::OversizedPayload {
                actual: len,
                max: MAX_ZAKURA_SERVICE_ID_BYTES,
            });
        }
        let bytes = read_exact_vec(reader, len)?;
        validate_service_id(&bytes)?;
        let service =
            String::from_utf8(bytes).map_err(|_| DiscoveryWireError::NonAsciiServiceId)?;
        services.push(ZakuraServiceId(service));
    }
    Ok(services)
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
        let mut bytes = [0u8; NODE_ID_BYTES];
        reader.read_exact(&mut bytes)?;
        let node_id = NodeId::from_bytes(&bytes).map_err(|_| DiscoveryWireError::InvalidNodeId)?;
        node_ids.push(node_id);
    }
    Ok(node_ids)
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
            DiscoveryMessage::GetServices {
                wanted_services: services,
                limit: 2,
                exclude_node_ids: excluded,
            },
            DiscoveryMessage::Services {
                records: vec![record, other],
            },
        ];

        for message in messages {
            let encoded = message.encode().expect("message encodes");
            assert_eq!(DiscoveryMessage::decode(&encoded).unwrap(), message);
        }
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
        assert!(DiscoveryMessage::Services { records }.encode().is_err());

        let wanted_services = (0..=MAX_SERVICES_PER_RECORD).map(service).collect();
        assert!(DiscoveryMessage::GetPeers {
            limit: 1,
            wanted_services,
            exclude_node_ids: Vec::new(),
        }
        .encode()
        .is_err());

        let exclude_node_ids = (0..=MAX_DISCOVERY_EXCLUDED_NODE_IDS)
            .map(|_| secret_key().public())
            .collect();
        assert!(DiscoveryMessage::GetServices {
            wanted_services: Vec::new(),
            limit: 1,
            exclude_node_ids,
        }
        .encode()
        .is_err());

        assert!(DiscoveryMessage::GetPeers {
            limit: u16::try_from(MAX_DISCOVERY_RECORDS_PER_RESPONSE + 1)
                .expect("test limit fits in u16"),
            wanted_services: Vec::new(),
            exclude_node_ids: Vec::new(),
        }
        .encode()
        .is_err());
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

        book.import_record(failed.clone(), None, NOW, &context())
            .unwrap();
        book.import_record(preferred.clone(), None, NOW, &context())
            .unwrap();
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
            book.import_record(record.clone(), None, NOW, &context())
                .expect("test record imports");
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

        book.import_record(failed.clone(), None, NOW, &context())
            .unwrap();
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
    fn discovery_book_services_and_persistence_hooks_revalidate_records() {
        let mut book = ZakuraDiscoveryBook::default();
        let record = signed_record_with(1, service(9), test_addr(9));
        let node_id = record.body.node_id;
        book.import_record(record.clone(), None, NOW, &context())
            .unwrap();
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
            .import_peer_records([discovered.clone(), general.clone()], None)
            .await;
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
        handle
            .import_peer_records([candidate.clone(), connected], None)
            .await;

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
        handle
            .import_peer_records([candidate.clone(), connected], None)
            .await;

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
