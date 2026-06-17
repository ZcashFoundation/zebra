//! Zakura P2P v2 handshake and bounded wire encodings.

use std::{
    cmp::min,
    collections::HashMap,
    io::{self, Cursor, Read, Write},
    time::{Duration, Instant},
};

use blake2b_simd::Params as Blake2bParams;
use byteorder::{LittleEndian, ReadBytesExt, WriteBytesExt};
use thiserror::Error;

use zebra_chain::{parameters::Network, serialization::ZcashSerialize};

use crate::{protocol::external::types::Nonce, VersionMessage};

/// The Zcash command used for Zakura upgrade prelude messages.
pub const P2P_V2_UPGRADE_COMMAND: &str = "p2pv2up";

/// The padded Zcash command used for Zakura upgrade prelude messages.
pub const P2P_V2_UPGRADE_COMMAND_BYTES: &[u8; 12] = b"p2pv2up\0\0\0\0\0";

/// The ALPN used by the first Zakura P2P v2 protocol version.
pub const P2P_V2_ALPN: &[u8] = b"p2p-v2/1";

/// Magic bytes for Zakura legacy upgrade prelude messages.
pub const PRELUDE_MAGIC: [u8; 8] = *b"ZAKURA1\0";

/// Magic bytes for Zakura control hello messages.
pub const CONTROL_HELLO_MAGIC: [u8; 8] = *b"ZAKCTRL\0";

/// Magic bytes for Zakura control acknowledgement messages.
pub const CONTROL_ACK_MAGIC: [u8; 8] = *b"ZAKACK\0\0";

/// Magic bytes for per-stream Zakura preludes.
pub const STREAM_PRELUDE_MAGIC: [u8; 4] = *b"ZKST";

/// The only prelude encoding version supported by this implementation.
pub const PRELUDE_VERSION: u16 = 1;

/// The only control handshake encoding version supported by this implementation.
pub const CONTROL_VERSION: u16 = 1;

/// The first Zakura wire protocol version.
pub const ZAKURA_PROTOCOL_VERSION_1: u16 = 1;

/// Hard cap for any legacy TCP upgrade prelude payload.
pub const MAX_PRELUDE_PAYLOAD_BYTES: usize = 4 * 1024;

/// Hard cap for any control handshake payload.
pub const MAX_CONTROL_PAYLOAD_BYTES: usize = 16 * 1024;

/// Maximum encoded Iroh node id length accepted in Zakura handshakes.
pub const MAX_IROH_NODE_ID_BYTES: usize = 128;

/// Maximum number of direct Iroh address hints in the legacy prelude.
pub const MAX_IROH_DIRECT_ADDRESSES: usize = 8;

/// Maximum length of one encoded direct Iroh address hint.
pub const MAX_IROH_DIRECT_ADDRESS_BYTES: usize = 512;

/// Maximum length of one encoded relay hint.
pub const MAX_IROH_RELAY_HINT_BYTES: usize = 512;

/// Maximum locally accepted control frame size.
pub const LOCAL_MAX_CONTROL_FRAME_BYTES: u32 = 1024 * 1024;

/// Maximum locally accepted application message size advertised in control hello.
pub const LOCAL_MAX_MESSAGE_BYTES: u32 = 4 * 1024 * 1024;

/// Maximum locally accepted open streams advertised in control hello.
pub const LOCAL_MAX_OPEN_STREAMS: u16 = 1024;

/// Maximum locally accepted inbound queue depth advertised in control hello.
pub const LOCAL_MAX_INBOUND_QUEUE_DEPTH: u16 = 4096;

/// Maximum locally accepted idle timeout advertised in control hello.
pub const LOCAL_MAX_IDLE_TIMEOUT_MILLIS: u32 = 10 * 60 * 1000;

/// The fixed output size for Zakura transcript bindings.
pub const TRANSCRIPT_HASH_BYTES: usize = 32;

const INIT_DISCRIMINATOR: u8 = 1;
const ACCEPT_DISCRIMINATOR: u8 = 2;
const REJECT_DISCRIMINATOR: u8 = 3;
pub(crate) const FRAME_HEADER_BYTES: usize = 2 + 2 + 4;

/// A bounded authenticated Zakura peer identity.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
#[cfg_attr(
    any(test, feature = "proptest-impl"),
    derive(proptest_derive::Arbitrary)
)]
pub struct ZakuraPeerId(Vec<u8>);

impl ZakuraPeerId {
    /// Creates a peer id after applying the Zakura node-id bound.
    pub fn new(bytes: impl Into<Vec<u8>>) -> Result<Self, ZakuraProtocolError> {
        let bytes = bytes.into();
        validate_bounded_bytes(&bytes, MAX_IROH_NODE_ID_BYTES, "iroh node id")?;
        Ok(Self(bytes))
    }

    /// Returns the encoded peer id bytes.
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }
}

/// A small stable network id for fast Zakura rejects.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
#[repr(u32)]
pub enum ZakuraNetworkId {
    /// Zcash Mainnet.
    Mainnet = 1,
    /// The default public Zcash Testnet.
    Testnet = 2,
    /// A local Regtest network.
    Regtest = 3,
    /// A configured test network.
    Configured = 4,
}

impl ZakuraNetworkId {
    /// Returns the Zakura network id for `network`.
    pub fn from_network(network: &Network) -> Self {
        match network {
            Network::Mainnet => Self::Mainnet,
            Network::Testnet(params) if params.is_regtest() => Self::Regtest,
            Network::Testnet(params) if params.is_default_testnet() => Self::Testnet,
            Network::Testnet(_) => Self::Configured,
        }
    }

    fn from_u32(value: u32) -> Result<Self, ZakuraProtocolError> {
        match value {
            1 => Ok(Self::Mainnet),
            2 => Ok(Self::Testnet),
            3 => Ok(Self::Regtest),
            4 => Ok(Self::Configured),
            _ => Err(ZakuraProtocolError::InvalidNetworkId(value)),
        }
    }

    /// Returns this network id's pinned wire value.
    pub fn code(self) -> u32 {
        // Safe: `ZakuraNetworkId` has `#[repr(u32)]`, so the cast uses the pinned wire value.
        self as u32
    }
}

/// Static local Zakura handshake policy derived from configuration.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct ZakuraHandshakeConfig {
    /// Supported prelude version.
    pub prelude_version: u16,
    /// Lowest supported Zakura wire protocol.
    pub zakura_protocol_min: u16,
    /// Highest supported Zakura wire protocol.
    pub zakura_protocol_max: u16,
    /// Local network id.
    pub network_id: ZakuraNetworkId,
    /// Local genesis block hash.
    pub chain_id: [u8; 32],
    /// Required peer capabilities.
    pub required_capabilities: u64,
    /// Locally supported capabilities.
    pub supported_capabilities: u64,
    /// Locally supported channel bits.
    pub supported_channels: u64,
    /// Largest control frame accepted locally.
    pub max_control_frame_bytes: u32,
    /// Largest application message accepted locally.
    pub max_message_bytes: u32,
    /// Largest open-stream count accepted locally.
    pub max_open_streams: u16,
    /// Largest inbound queue depth accepted locally.
    pub max_inbound_queue_depth: u16,
    /// Largest idle timeout accepted locally.
    pub max_idle_timeout_millis: u32,
}

impl ZakuraHandshakeConfig {
    /// Returns the conservative default-off Zakura v1 local policy for `network`.
    pub fn for_network(network: &Network) -> Self {
        Self {
            prelude_version: PRELUDE_VERSION,
            zakura_protocol_min: ZAKURA_PROTOCOL_VERSION_1,
            zakura_protocol_max: ZAKURA_PROTOCOL_VERSION_1,
            network_id: ZakuraNetworkId::from_network(network),
            chain_id: network.genesis_hash().0,
            required_capabilities: 0,
            supported_capabilities: 0,
            supported_channels: 0,
            max_control_frame_bytes: LOCAL_MAX_CONTROL_FRAME_BYTES,
            max_message_bytes: LOCAL_MAX_MESSAGE_BYTES,
            max_open_streams: LOCAL_MAX_OPEN_STREAMS,
            max_inbound_queue_depth: LOCAL_MAX_INBOUND_QUEUE_DEPTH,
            max_idle_timeout_millis: LOCAL_MAX_IDLE_TIMEOUT_MILLIS,
        }
    }

    /// Returns the network label used by low-cardinality metrics.
    pub fn network_label(&self) -> &'static str {
        match self.network_id {
            ZakuraNetworkId::Mainnet => "mainnet",
            ZakuraNetworkId::Testnet => "testnet",
            ZakuraNetworkId::Regtest => "regtest",
            ZakuraNetworkId::Configured => "configured",
        }
    }
}

/// The legacy Zebra nonces as observed locally.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct ZakuraLegacyNonces {
    /// The nonce sent by this node in its legacy `version` message.
    pub local_zebra_nonce: Nonce,
    /// The nonce received from the peer in its legacy `version` message.
    pub remote_zebra_nonce: Nonce,
}

/// Why a well-formed Zakura upgrade was rejected.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
#[repr(u16)]
pub enum ZakuraRejectReason {
    /// Unsupported prelude version.
    UnsupportedPreludeVersion = 1,
    /// No compatible Zakura protocol version exists.
    IncompatibleZakuraProtocol = 2,
    /// The peer is on a different network.
    WrongNetwork = 3,
    /// The peer is on a different chain.
    WrongChain = 4,
    /// A required capability was missing.
    MissingRequiredCapability = 5,
    /// A peer-advertised resource limit was unusable or too large.
    ResourceLimit = 6,
    /// The authenticated peer is already connected.
    AlreadyConnected = 7,
    /// The local node cannot accept this upgrade now.
    TemporaryUnavailable = 8,
}

impl ZakuraRejectReason {
    fn from_u16(value: u16) -> Result<Self, ZakuraProtocolError> {
        match value {
            1 => Ok(Self::UnsupportedPreludeVersion),
            2 => Ok(Self::IncompatibleZakuraProtocol),
            3 => Ok(Self::WrongNetwork),
            4 => Ok(Self::WrongChain),
            5 => Ok(Self::MissingRequiredCapability),
            6 => Ok(Self::ResourceLimit),
            7 => Ok(Self::AlreadyConnected),
            8 => Ok(Self::TemporaryUnavailable),
            _ => Err(ZakuraProtocolError::InvalidRejectReason(value)),
        }
    }

    fn code(self) -> u16 {
        // Safe: `ZakuraRejectReason` has `#[repr(u16)]`, so the cast uses the pinned wire value.
        self as u16
    }
}

/// Whether a Zakura validation failure is neutral or may indicate abuse.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum ZakuraFailureClass {
    /// A routine mismatch or resource condition that should close neutrally.
    Neutral,
    /// A malformed or forged handshake condition that may be punitive if repeated.
    PotentiallyPunitive,
}

/// Internal validation failures after the legacy wire reject boundary.
///
/// Unlike [`ZakuraRejectReason`], these variants are not serialized on the wire.
/// They preserve the failure-policy distinction needed by the control handshake.
#[derive(Copy, Clone, Debug, Eq, Error, PartialEq)]
pub enum ZakuraValidationError {
    /// Unsupported control or prelude version.
    #[error("unsupported Zakura {0} version")]
    UnsupportedVersion(&'static str),

    /// No compatible Zakura protocol version exists.
    #[error("incompatible Zakura protocol")]
    IncompatibleZakuraProtocol,

    /// The peer is on a different network.
    #[error("wrong Zakura network")]
    WrongNetwork,

    /// The peer is on a different chain.
    #[error("wrong Zakura chain")]
    WrongChain,

    /// A required capability or channel is missing.
    #[error("missing required Zakura capability or channel")]
    MissingRequiredCapability,

    /// A peer-advertised resource limit was unusable or too large.
    #[error("invalid Zakura resource limit")]
    ResourceLimit,

    /// Authenticated Iroh identity does not match the handshake.
    #[error("authenticated Zakura identity mismatch")]
    IdentityMismatch,

    /// The legacy upgrade transcript does not match.
    #[error("Zakura legacy upgrade transcript mismatch")]
    TranscriptMismatch,

    /// One or both upgrade nonces do not match.
    #[error("Zakura upgrade nonce mismatch")]
    UpgradeNonceMismatch,

    /// The control handshake magic or path is malformed.
    #[error("malformed Zakura control handshake")]
    MalformedControl,

    /// The control acknowledgement did not bind to the exact nonce exchange.
    #[error("Zakura control nonce mismatch")]
    ControlNonceMismatch,
}

impl ZakuraValidationError {
    /// Returns the failure-policy class for this validation error.
    pub fn failure_class(self) -> ZakuraFailureClass {
        match self {
            Self::MalformedControl
            | Self::IdentityMismatch
            | Self::TranscriptMismatch
            | Self::UpgradeNonceMismatch
            | Self::ControlNonceMismatch => ZakuraFailureClass::PotentiallyPunitive,

            Self::UnsupportedVersion(_)
            | Self::IncompatibleZakuraProtocol
            | Self::WrongNetwork
            | Self::WrongChain
            | Self::MissingRequiredCapability
            | Self::ResourceLimit => ZakuraFailureClass::Neutral,
        }
    }

    fn from_wire_reject(reason: ZakuraRejectReason) -> Self {
        match reason {
            ZakuraRejectReason::UnsupportedPreludeVersion => Self::UnsupportedVersion("prelude"),
            ZakuraRejectReason::IncompatibleZakuraProtocol => Self::IncompatibleZakuraProtocol,
            ZakuraRejectReason::WrongNetwork => Self::WrongNetwork,
            ZakuraRejectReason::WrongChain => Self::WrongChain,
            ZakuraRejectReason::MissingRequiredCapability => Self::MissingRequiredCapability,
            ZakuraRejectReason::ResourceLimit => Self::ResourceLimit,
            ZakuraRejectReason::AlreadyConnected | ZakuraRejectReason::TemporaryUnavailable => {
                Self::MalformedControl
            }
        }
    }
}

/// A decoded legacy TCP Zakura upgrade message.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum P2pV2Upgrade {
    /// The TCP initiator's upgrade prelude.
    Init(P2pV2UpgradeInit),
    /// The TCP responder's acceptance prelude.
    Accept(P2pV2UpgradeAccept),
    /// The TCP responder's neutral rejection.
    Reject(P2pV2UpgradeReject),
}

impl P2pV2Upgrade {
    /// Encodes this prelude message and enforces the hard prelude cap.
    pub fn encode(&self) -> Result<Vec<u8>, ZakuraProtocolError> {
        let mut bytes = Vec::new();
        match self {
            Self::Init(init) => {
                bytes.write_u8(INIT_DISCRIMINATOR)?;
                init.encode_to(&mut bytes)?;
            }
            Self::Accept(accept) => {
                bytes.write_u8(ACCEPT_DISCRIMINATOR)?;
                accept.encode_to(&mut bytes)?;
            }
            Self::Reject(reject) => {
                bytes.write_u8(REJECT_DISCRIMINATOR)?;
                reject.encode_to(&mut bytes)?;
            }
        }
        if bytes.len() > MAX_PRELUDE_PAYLOAD_BYTES {
            return Err(ZakuraProtocolError::OversizedPayload {
                actual: bytes.len(),
                max: MAX_PRELUDE_PAYLOAD_BYTES,
            });
        }
        Ok(bytes)
    }

    /// Decodes a prelude message after applying the hard prelude cap.
    pub fn decode(bytes: &[u8]) -> Result<Self, ZakuraProtocolError> {
        if bytes.len() > MAX_PRELUDE_PAYLOAD_BYTES {
            return Err(ZakuraProtocolError::OversizedPayload {
                actual: bytes.len(),
                max: MAX_PRELUDE_PAYLOAD_BYTES,
            });
        }

        let mut reader = Cursor::new(bytes);
        let message = match reader.read_u8()? {
            INIT_DISCRIMINATOR => Self::Init(P2pV2UpgradeInit::decode_from(&mut reader)?),
            ACCEPT_DISCRIMINATOR => Self::Accept(P2pV2UpgradeAccept::decode_from(&mut reader)?),
            REJECT_DISCRIMINATOR => Self::Reject(P2pV2UpgradeReject::decode_from(&mut reader)?),
            value => return Err(ZakuraProtocolError::InvalidDiscriminator(value)),
        };
        reject_trailing(bytes, &reader)?;
        Ok(message)
    }
}

/// The TCP initiator's legacy upgrade prelude.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct P2pV2UpgradeInit {
    /// Prelude magic bytes.
    pub magic: [u8; 8],
    /// Prelude encoding version.
    pub prelude_version: u16,
    /// Lowest supported Zakura protocol version.
    pub zakura_protocol_min: u16,
    /// Highest supported Zakura protocol version.
    pub zakura_protocol_max: u16,
    /// Local network id.
    pub network_id: ZakuraNetworkId,
    /// Local genesis block hash.
    pub chain_id: [u8; 32],
    /// High-level Zakura capabilities.
    pub capabilities: u64,
    /// The nonce this side sent in its legacy `version`.
    pub local_zebra_nonce: Nonce,
    /// The nonce this side received in the remote legacy `version`.
    pub remote_zebra_nonce: Nonce,
    /// Fresh nonce for transcript binding.
    pub upgrade_nonce: [u8; 32],
    /// Expected Iroh identity for this side.
    pub iroh_node_id: Vec<u8>,
    /// Direct Iroh dial hints.
    pub iroh_direct_addresses: Vec<Vec<u8>>,
    /// Optional relay hint.
    pub iroh_relay_hint: Option<Vec<u8>>,
    /// Local receive cap for control frames.
    pub max_control_frame_bytes: u32,
    /// Local initial open-stream limit.
    pub max_open_streams: u16,
}

impl P2pV2UpgradeInit {
    /// Validates this init from the responder's perspective.
    pub fn validate(
        &self,
        local: &ZakuraHandshakeConfig,
        nonces: ZakuraLegacyNonces,
    ) -> Result<u16, ZakuraRejectReason> {
        validate_prelude_static_fields(
            PreludeStaticFields {
                magic: self.magic,
                prelude_version: self.prelude_version,
                network_id: self.network_id,
                chain_id: self.chain_id,
                capabilities: self.capabilities,
                iroh_node_id: &self.iroh_node_id,
                iroh_direct_addresses: &self.iroh_direct_addresses,
                iroh_relay_hint: self.iroh_relay_hint.as_deref(),
                max_control_frame_bytes: self.max_control_frame_bytes,
                max_open_streams: self.max_open_streams,
            },
            local,
        )?;

        validate_peer_nonce_labels(self.local_zebra_nonce, self.remote_zebra_nonce, nonces)?;

        select_zakura_protocol(
            local.zakura_protocol_min,
            local.zakura_protocol_max,
            self.zakura_protocol_min,
            self.zakura_protocol_max,
        )
    }

    fn encode_to<W: Write>(&self, writer: &mut W) -> Result<(), ZakuraProtocolError> {
        writer.write_all(&self.magic)?;
        writer.write_u16::<LittleEndian>(self.prelude_version)?;
        writer.write_u16::<LittleEndian>(self.zakura_protocol_min)?;
        writer.write_u16::<LittleEndian>(self.zakura_protocol_max)?;
        writer.write_u32::<LittleEndian>(self.network_id.code())?;
        writer.write_all(&self.chain_id)?;
        writer.write_u64::<LittleEndian>(self.capabilities)?;
        writer.write_u64::<LittleEndian>(self.local_zebra_nonce.0)?;
        writer.write_u64::<LittleEndian>(self.remote_zebra_nonce.0)?;
        writer.write_all(&self.upgrade_nonce)?;
        write_bounded_bytes(writer, &self.iroh_node_id, MAX_IROH_NODE_ID_BYTES)?;
        write_bounded_bytes_list(
            writer,
            &self.iroh_direct_addresses,
            MAX_IROH_DIRECT_ADDRESSES,
            MAX_IROH_DIRECT_ADDRESS_BYTES,
        )?;
        write_optional_bounded_bytes(
            writer,
            self.iroh_relay_hint.as_deref(),
            MAX_IROH_RELAY_HINT_BYTES,
        )?;
        writer.write_u32::<LittleEndian>(self.max_control_frame_bytes)?;
        writer.write_u16::<LittleEndian>(self.max_open_streams)?;
        Ok(())
    }

    fn decode_from<R: Read>(reader: &mut R) -> Result<Self, ZakuraProtocolError> {
        let mut magic = [0; 8];
        reader.read_exact(&mut magic)?;
        let prelude_version = reader.read_u16::<LittleEndian>()?;
        let zakura_protocol_min = reader.read_u16::<LittleEndian>()?;
        let zakura_protocol_max = reader.read_u16::<LittleEndian>()?;
        let network_id = ZakuraNetworkId::from_u32(reader.read_u32::<LittleEndian>()?)?;
        let mut chain_id = [0; 32];
        reader.read_exact(&mut chain_id)?;
        let capabilities = reader.read_u64::<LittleEndian>()?;
        let local_zebra_nonce = Nonce(reader.read_u64::<LittleEndian>()?);
        let remote_zebra_nonce = Nonce(reader.read_u64::<LittleEndian>()?);
        let mut upgrade_nonce = [0; 32];
        reader.read_exact(&mut upgrade_nonce)?;
        let iroh_node_id = read_bounded_bytes(reader, MAX_IROH_NODE_ID_BYTES)?;
        let iroh_direct_addresses = read_bounded_bytes_list(
            reader,
            MAX_IROH_DIRECT_ADDRESSES,
            MAX_IROH_DIRECT_ADDRESS_BYTES,
        )?;
        let iroh_relay_hint = read_optional_bounded_bytes(reader, MAX_IROH_RELAY_HINT_BYTES)?;
        let max_control_frame_bytes = reader.read_u32::<LittleEndian>()?;
        let max_open_streams = reader.read_u16::<LittleEndian>()?;

        Ok(Self {
            magic,
            prelude_version,
            zakura_protocol_min,
            zakura_protocol_max,
            network_id,
            chain_id,
            capabilities,
            local_zebra_nonce,
            remote_zebra_nonce,
            upgrade_nonce,
            iroh_node_id,
            iroh_direct_addresses,
            iroh_relay_hint,
            max_control_frame_bytes,
            max_open_streams,
        })
    }
}

/// The TCP responder's legacy upgrade acceptance prelude.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct P2pV2UpgradeAccept {
    /// Prelude magic bytes.
    pub magic: [u8; 8],
    /// Prelude encoding version.
    pub prelude_version: u16,
    /// Selected Zakura protocol version.
    pub selected_zakura_protocol: u16,
    /// Local network id.
    pub network_id: ZakuraNetworkId,
    /// Local genesis block hash.
    pub chain_id: [u8; 32],
    /// High-level Zakura capabilities.
    pub capabilities: u64,
    /// The initiator nonce echoed from the init.
    pub initiator_upgrade_nonce: [u8; 32],
    /// Fresh responder nonce for transcript binding.
    pub responder_upgrade_nonce: [u8; 32],
    /// The nonce this side sent in its legacy `version`.
    pub local_zebra_nonce: Nonce,
    /// The nonce this side received in the remote legacy `version`.
    pub remote_zebra_nonce: Nonce,
    /// Expected Iroh identity for this side.
    pub iroh_node_id: Vec<u8>,
    /// Direct Iroh dial hints.
    pub iroh_direct_addresses: Vec<Vec<u8>>,
    /// Optional relay hint.
    pub iroh_relay_hint: Option<Vec<u8>>,
    /// Local receive cap for control frames.
    pub max_control_frame_bytes: u32,
    /// Local initial open-stream limit.
    pub max_open_streams: u16,
}

impl P2pV2UpgradeAccept {
    /// Validates this accept from the initiator's perspective.
    pub fn validate(
        &self,
        local: &ZakuraHandshakeConfig,
        nonces: ZakuraLegacyNonces,
        init: &P2pV2UpgradeInit,
    ) -> Result<(), ZakuraValidationError> {
        validate_prelude_static_fields(
            PreludeStaticFields {
                magic: self.magic,
                prelude_version: self.prelude_version,
                network_id: self.network_id,
                chain_id: self.chain_id,
                capabilities: self.capabilities,
                iroh_node_id: &self.iroh_node_id,
                iroh_direct_addresses: &self.iroh_direct_addresses,
                iroh_relay_hint: self.iroh_relay_hint.as_deref(),
                max_control_frame_bytes: self.max_control_frame_bytes,
                max_open_streams: self.max_open_streams,
            },
            local,
        )
        .map_err(ZakuraValidationError::from_wire_reject)?;
        validate_peer_nonce_labels(self.local_zebra_nonce, self.remote_zebra_nonce, nonces)
            .map_err(|_| ZakuraValidationError::UpgradeNonceMismatch)?;
        if self.initiator_upgrade_nonce != init.upgrade_nonce {
            return Err(ZakuraValidationError::UpgradeNonceMismatch);
        }

        let selected = select_zakura_protocol(
            local.zakura_protocol_min,
            local.zakura_protocol_max,
            init.zakura_protocol_min,
            init.zakura_protocol_max,
        )
        .map_err(ZakuraValidationError::from_wire_reject)?;
        if self.selected_zakura_protocol != selected {
            return Err(ZakuraValidationError::IncompatibleZakuraProtocol);
        }

        Ok(())
    }

    fn encode_to<W: Write>(&self, writer: &mut W) -> Result<(), ZakuraProtocolError> {
        writer.write_all(&self.magic)?;
        writer.write_u16::<LittleEndian>(self.prelude_version)?;
        writer.write_u16::<LittleEndian>(self.selected_zakura_protocol)?;
        writer.write_u32::<LittleEndian>(self.network_id.code())?;
        writer.write_all(&self.chain_id)?;
        writer.write_u64::<LittleEndian>(self.capabilities)?;
        writer.write_all(&self.initiator_upgrade_nonce)?;
        writer.write_all(&self.responder_upgrade_nonce)?;
        writer.write_u64::<LittleEndian>(self.local_zebra_nonce.0)?;
        writer.write_u64::<LittleEndian>(self.remote_zebra_nonce.0)?;
        write_bounded_bytes(writer, &self.iroh_node_id, MAX_IROH_NODE_ID_BYTES)?;
        write_bounded_bytes_list(
            writer,
            &self.iroh_direct_addresses,
            MAX_IROH_DIRECT_ADDRESSES,
            MAX_IROH_DIRECT_ADDRESS_BYTES,
        )?;
        write_optional_bounded_bytes(
            writer,
            self.iroh_relay_hint.as_deref(),
            MAX_IROH_RELAY_HINT_BYTES,
        )?;
        writer.write_u32::<LittleEndian>(self.max_control_frame_bytes)?;
        writer.write_u16::<LittleEndian>(self.max_open_streams)?;
        Ok(())
    }

    fn decode_from<R: Read>(reader: &mut R) -> Result<Self, ZakuraProtocolError> {
        let mut magic = [0; 8];
        reader.read_exact(&mut magic)?;
        let prelude_version = reader.read_u16::<LittleEndian>()?;
        let selected_zakura_protocol = reader.read_u16::<LittleEndian>()?;
        let network_id = ZakuraNetworkId::from_u32(reader.read_u32::<LittleEndian>()?)?;
        let mut chain_id = [0; 32];
        reader.read_exact(&mut chain_id)?;
        let capabilities = reader.read_u64::<LittleEndian>()?;
        let mut initiator_upgrade_nonce = [0; 32];
        reader.read_exact(&mut initiator_upgrade_nonce)?;
        let mut responder_upgrade_nonce = [0; 32];
        reader.read_exact(&mut responder_upgrade_nonce)?;
        let local_zebra_nonce = Nonce(reader.read_u64::<LittleEndian>()?);
        let remote_zebra_nonce = Nonce(reader.read_u64::<LittleEndian>()?);
        let iroh_node_id = read_bounded_bytes(reader, MAX_IROH_NODE_ID_BYTES)?;
        let iroh_direct_addresses = read_bounded_bytes_list(
            reader,
            MAX_IROH_DIRECT_ADDRESSES,
            MAX_IROH_DIRECT_ADDRESS_BYTES,
        )?;
        let iroh_relay_hint = read_optional_bounded_bytes(reader, MAX_IROH_RELAY_HINT_BYTES)?;
        let max_control_frame_bytes = reader.read_u32::<LittleEndian>()?;
        let max_open_streams = reader.read_u16::<LittleEndian>()?;

        Ok(Self {
            magic,
            prelude_version,
            selected_zakura_protocol,
            network_id,
            chain_id,
            capabilities,
            initiator_upgrade_nonce,
            responder_upgrade_nonce,
            local_zebra_nonce,
            remote_zebra_nonce,
            iroh_node_id,
            iroh_direct_addresses,
            iroh_relay_hint,
            max_control_frame_bytes,
            max_open_streams,
        })
    }
}

/// The TCP responder's legacy upgrade rejection.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct P2pV2UpgradeReject {
    /// Prelude magic bytes.
    pub magic: [u8; 8],
    /// Prelude encoding version.
    pub prelude_version: u16,
    /// Neutral rejection reason.
    pub reason: ZakuraRejectReason,
}

impl P2pV2UpgradeReject {
    fn encode_to<W: Write>(&self, writer: &mut W) -> Result<(), ZakuraProtocolError> {
        writer.write_all(&self.magic)?;
        writer.write_u16::<LittleEndian>(self.prelude_version)?;
        writer.write_u16::<LittleEndian>(self.reason.code())?;
        Ok(())
    }

    fn decode_from<R: Read>(reader: &mut R) -> Result<Self, ZakuraProtocolError> {
        let mut magic = [0; 8];
        reader.read_exact(&mut magic)?;
        let prelude_version = reader.read_u16::<LittleEndian>()?;
        let reason = ZakuraRejectReason::from_u16(reader.read_u16::<LittleEndian>()?)?;
        Ok(Self {
            magic,
            prelude_version,
            reason,
        })
    }
}

/// The role in the control handshake.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum ZakuraControlRole {
    /// The side that initiated the legacy TCP upgrade or native dial.
    Initiator = 1,
    /// The side that responded to the legacy TCP upgrade or accepted the native dial.
    Responder = 2,
}

impl ZakuraControlRole {
    fn from_u8(value: u8) -> Result<Self, ZakuraProtocolError> {
        match value {
            1 => Ok(Self::Initiator),
            2 => Ok(Self::Responder),
            _ => Err(ZakuraProtocolError::InvalidRole(value)),
        }
    }

    fn code(self) -> u8 {
        // Safe: `ZakuraControlRole` has `#[repr(u8)]`, so the cast uses the pinned wire value.
        self as u8
    }
}

/// Whether the control handshake is bound to a legacy upgrade transcript.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum ZakuraHandshakePath {
    /// Legacy TCP upgrade path, with transcript binding required.
    Upgraded = 0,
    /// Native Iroh bootstrap path, with transcript binding skipped.
    Native = 1,
}

impl ZakuraHandshakePath {
    fn from_u8(value: u8) -> Result<Self, ZakuraProtocolError> {
        match value {
            0 => Ok(Self::Upgraded),
            1 => Ok(Self::Native),
            _ => Err(ZakuraProtocolError::InvalidHandshakePath(value)),
        }
    }

    fn code(self) -> u8 {
        // Safe: `ZakuraHandshakePath` has `#[repr(u8)]`, so the cast uses the pinned wire value.
        self as u8
    }
}

/// Bounded resource limits exchanged during the Zakura control handshake.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct ZakuraLimits {
    /// Maximum frame bytes.
    pub max_frame_bytes: u32,
    /// Maximum full message bytes.
    pub max_message_bytes: u32,
    /// Maximum open streams.
    pub max_open_streams: u16,
    /// Maximum inbound queue depth.
    pub max_inbound_queue_depth: u16,
    /// Idle timeout in milliseconds.
    pub idle_timeout_millis: u32,
}

/// Initial limits advertised in a Zakura control hello.
pub type ZakuraInitialLimits = ZakuraLimits;

/// Accepted limits sent in a Zakura control ack.
pub type ZakuraAcceptedLimits = ZakuraLimits;

/// The authenticated Zakura control hello.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ZakuraControlHello {
    /// Control hello magic bytes.
    pub magic: [u8; 8],
    /// Control encoding version.
    pub control_version: u16,
    /// Selected Zakura protocol version.
    pub selected_zakura_protocol: u16,
    /// Native or upgraded handshake path.
    pub handshake_path: ZakuraHandshakePath,
    /// Control role.
    pub role: ZakuraControlRole,
    /// Local network id.
    pub network_id: ZakuraNetworkId,
    /// Local genesis block hash.
    pub chain_id: [u8; 32],
    /// Authenticated Iroh node id this peer claims.
    pub iroh_node_id: Vec<u8>,
    /// Fresh nonce for this control exchange.
    pub peer_nonce: [u8; 32],
    /// Initiator upgrade nonce, or zeroes for native handshakes.
    pub initiator_upgrade_nonce: [u8; 32],
    /// Responder upgrade nonce, or zeroes for native handshakes.
    pub responder_upgrade_nonce: [u8; 32],
    /// Legacy upgrade transcript hash, or zeroes for native handshakes.
    pub legacy_upgrade_transcript: [u8; 32],
    /// High-level Zakura capabilities.
    pub capabilities: u64,
    /// Channel bits required by this peer.
    pub required_channels: u64,
    /// Initial resource limits.
    pub initial_limits: ZakuraInitialLimits,
}

impl ZakuraControlHello {
    /// Encodes this control hello with the hard control cap.
    pub fn encode(&self) -> Result<Vec<u8>, ZakuraProtocolError> {
        let mut bytes = Vec::new();
        bytes.write_all(&self.magic)?;
        bytes.write_u16::<LittleEndian>(self.control_version)?;
        bytes.write_u16::<LittleEndian>(self.selected_zakura_protocol)?;
        bytes.write_u8(self.handshake_path.code())?;
        bytes.write_u8(self.role.code())?;
        bytes.write_u32::<LittleEndian>(self.network_id.code())?;
        bytes.write_all(&self.chain_id)?;
        write_bounded_bytes(&mut bytes, &self.iroh_node_id, MAX_IROH_NODE_ID_BYTES)?;
        bytes.write_all(&self.peer_nonce)?;
        bytes.write_all(&self.initiator_upgrade_nonce)?;
        bytes.write_all(&self.responder_upgrade_nonce)?;
        bytes.write_all(&self.legacy_upgrade_transcript)?;
        bytes.write_u64::<LittleEndian>(self.capabilities)?;
        bytes.write_u64::<LittleEndian>(self.required_channels)?;
        self.initial_limits.encode_to(&mut bytes)?;
        if bytes.len() > MAX_CONTROL_PAYLOAD_BYTES {
            return Err(ZakuraProtocolError::OversizedPayload {
                actual: bytes.len(),
                max: MAX_CONTROL_PAYLOAD_BYTES,
            });
        }
        Ok(bytes)
    }

    /// Decodes this control hello with the hard control cap.
    pub fn decode(bytes: &[u8]) -> Result<Self, ZakuraProtocolError> {
        if bytes.len() > MAX_CONTROL_PAYLOAD_BYTES {
            return Err(ZakuraProtocolError::OversizedPayload {
                actual: bytes.len(),
                max: MAX_CONTROL_PAYLOAD_BYTES,
            });
        }
        let mut reader = Cursor::new(bytes);
        let mut magic = [0; 8];
        reader.read_exact(&mut magic)?;
        let control_version = reader.read_u16::<LittleEndian>()?;
        let selected_zakura_protocol = reader.read_u16::<LittleEndian>()?;
        let handshake_path = ZakuraHandshakePath::from_u8(reader.read_u8()?)?;
        let role = ZakuraControlRole::from_u8(reader.read_u8()?)?;
        let network_id = ZakuraNetworkId::from_u32(reader.read_u32::<LittleEndian>()?)?;
        let mut chain_id = [0; 32];
        reader.read_exact(&mut chain_id)?;
        let iroh_node_id = read_bounded_bytes(&mut reader, MAX_IROH_NODE_ID_BYTES)?;
        let mut peer_nonce = [0; 32];
        reader.read_exact(&mut peer_nonce)?;
        let mut initiator_upgrade_nonce = [0; 32];
        reader.read_exact(&mut initiator_upgrade_nonce)?;
        let mut responder_upgrade_nonce = [0; 32];
        reader.read_exact(&mut responder_upgrade_nonce)?;
        let mut legacy_upgrade_transcript = [0; 32];
        reader.read_exact(&mut legacy_upgrade_transcript)?;
        let capabilities = reader.read_u64::<LittleEndian>()?;
        let required_channels = reader.read_u64::<LittleEndian>()?;
        let initial_limits = ZakuraInitialLimits::decode_from(&mut reader)?;
        reject_trailing(bytes, &reader)?;

        Ok(Self {
            magic,
            control_version,
            selected_zakura_protocol,
            handshake_path,
            role,
            network_id,
            chain_id,
            iroh_node_id,
            peer_nonce,
            initiator_upgrade_nonce,
            responder_upgrade_nonce,
            legacy_upgrade_transcript,
            capabilities,
            required_channels,
            initial_limits,
        })
    }

    /// Validates a peer hello against authenticated Iroh identity and local policy.
    pub fn validate(
        &self,
        expected: &ZakuraControlValidation<'_>,
    ) -> Result<(), ZakuraValidationError> {
        if self.magic != CONTROL_HELLO_MAGIC {
            return Err(ZakuraValidationError::MalformedControl);
        }
        if self.control_version != CONTROL_VERSION {
            return Err(ZakuraValidationError::UnsupportedVersion("control"));
        }
        if self.selected_zakura_protocol != expected.selected_zakura_protocol {
            return Err(ZakuraValidationError::IncompatibleZakuraProtocol);
        }
        if self.handshake_path != expected.handshake_path {
            return Err(ZakuraValidationError::MalformedControl);
        }
        if self.role != expected.remote_role {
            return Err(ZakuraValidationError::MalformedControl);
        }
        if self.network_id != expected.local.network_id {
            return Err(ZakuraValidationError::WrongNetwork);
        }
        if self.chain_id != expected.local.chain_id {
            return Err(ZakuraValidationError::WrongChain);
        }
        if self.iroh_node_id.as_slice() != expected.authenticated_remote_id {
            return Err(ZakuraValidationError::IdentityMismatch);
        }
        if self.capabilities & expected.local.required_capabilities
            != expected.local.required_capabilities
        {
            return Err(ZakuraValidationError::MissingRequiredCapability);
        }
        if self.required_channels & !expected.local.supported_channels != 0 {
            return Err(ZakuraValidationError::MissingRequiredCapability);
        }
        validate_initial_limits(self.initial_limits, expected.local)
            .map_err(ZakuraValidationError::from_wire_reject)?;

        match expected.handshake_path {
            ZakuraHandshakePath::Upgraded => {
                if self.initiator_upgrade_nonce != expected.initiator_upgrade_nonce
                    || self.responder_upgrade_nonce != expected.responder_upgrade_nonce
                {
                    return Err(ZakuraValidationError::UpgradeNonceMismatch);
                }
                if self.legacy_upgrade_transcript != expected.legacy_upgrade_transcript {
                    return Err(ZakuraValidationError::TranscriptMismatch);
                }
            }
            ZakuraHandshakePath::Native => {
                if self.initiator_upgrade_nonce != [0; 32]
                    || self.responder_upgrade_nonce != [0; 32]
                {
                    return Err(ZakuraValidationError::UpgradeNonceMismatch);
                }
                if self.legacy_upgrade_transcript != [0; 32] {
                    return Err(ZakuraValidationError::TranscriptMismatch);
                }
            }
        }

        Ok(())
    }
}

/// Inputs needed to validate a Zakura control hello.
#[derive(Copy, Clone, Debug)]
pub struct ZakuraControlValidation<'a> {
    /// Local Zakura policy.
    pub local: &'a ZakuraHandshakeConfig,
    /// The authenticated Iroh remote node id.
    pub authenticated_remote_id: &'a [u8],
    /// Selected Zakura protocol.
    pub selected_zakura_protocol: u16,
    /// Native or upgraded handshake path.
    pub handshake_path: ZakuraHandshakePath,
    /// Expected role claimed by the remote peer.
    pub remote_role: ZakuraControlRole,
    /// Expected initiator upgrade nonce.
    pub initiator_upgrade_nonce: [u8; 32],
    /// Expected responder upgrade nonce.
    pub responder_upgrade_nonce: [u8; 32],
    /// Expected legacy upgrade transcript hash.
    pub legacy_upgrade_transcript: [u8; 32],
}

/// The authenticated Zakura control acknowledgement.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ZakuraControlAck {
    /// Control ack magic bytes.
    pub magic: [u8; 8],
    /// Control encoding version.
    pub control_version: u16,
    /// Selected Zakura protocol version.
    pub selected_zakura_protocol: u16,
    /// This peer's control nonce.
    pub peer_nonce: [u8; 32],
    /// The nonce from the remote peer's control hello.
    pub remote_peer_nonce: [u8; 32],
    /// Capabilities accepted by this peer.
    pub accepted_capabilities: u64,
    /// Channels accepted by this peer.
    pub accepted_channels: u64,
    /// Accepted resource limits.
    pub accepted_limits: ZakuraAcceptedLimits,
}

impl ZakuraControlAck {
    /// Encodes this control ack with the hard control cap.
    pub fn encode(&self) -> Result<Vec<u8>, ZakuraProtocolError> {
        let mut bytes = Vec::new();
        bytes.write_all(&self.magic)?;
        bytes.write_u16::<LittleEndian>(self.control_version)?;
        bytes.write_u16::<LittleEndian>(self.selected_zakura_protocol)?;
        bytes.write_all(&self.peer_nonce)?;
        bytes.write_all(&self.remote_peer_nonce)?;
        bytes.write_u64::<LittleEndian>(self.accepted_capabilities)?;
        bytes.write_u64::<LittleEndian>(self.accepted_channels)?;
        self.accepted_limits.encode_to(&mut bytes)?;
        if bytes.len() > MAX_CONTROL_PAYLOAD_BYTES {
            return Err(ZakuraProtocolError::OversizedPayload {
                actual: bytes.len(),
                max: MAX_CONTROL_PAYLOAD_BYTES,
            });
        }
        Ok(bytes)
    }

    /// Decodes this control ack with the hard control cap.
    pub fn decode(bytes: &[u8]) -> Result<Self, ZakuraProtocolError> {
        if bytes.len() > MAX_CONTROL_PAYLOAD_BYTES {
            return Err(ZakuraProtocolError::OversizedPayload {
                actual: bytes.len(),
                max: MAX_CONTROL_PAYLOAD_BYTES,
            });
        }
        let mut reader = Cursor::new(bytes);
        let mut magic = [0; 8];
        reader.read_exact(&mut magic)?;
        let control_version = reader.read_u16::<LittleEndian>()?;
        let selected_zakura_protocol = reader.read_u16::<LittleEndian>()?;
        let mut peer_nonce = [0; 32];
        reader.read_exact(&mut peer_nonce)?;
        let mut remote_peer_nonce = [0; 32];
        reader.read_exact(&mut remote_peer_nonce)?;
        let accepted_capabilities = reader.read_u64::<LittleEndian>()?;
        let accepted_channels = reader.read_u64::<LittleEndian>()?;
        let accepted_limits = ZakuraAcceptedLimits::decode_from(&mut reader)?;
        reject_trailing(bytes, &reader)?;

        Ok(Self {
            magic,
            control_version,
            selected_zakura_protocol,
            peer_nonce,
            remote_peer_nonce,
            accepted_capabilities,
            accepted_channels,
            accepted_limits,
        })
    }

    /// Validates this acknowledgement against the exact control exchange.
    pub fn validate(
        &self,
        selected_zakura_protocol: u16,
        local_peer_nonce: [u8; 32],
        remote_peer_nonce: [u8; 32],
        requested_limits: &ZakuraInitialLimits,
        local: &ZakuraHandshakeConfig,
    ) -> Result<(), ZakuraValidationError> {
        if self.magic != CONTROL_ACK_MAGIC || self.control_version != CONTROL_VERSION {
            return Err(ZakuraValidationError::MalformedControl);
        }
        if self.selected_zakura_protocol != selected_zakura_protocol {
            return Err(ZakuraValidationError::IncompatibleZakuraProtocol);
        }
        if self.remote_peer_nonce != local_peer_nonce || self.peer_nonce != remote_peer_nonce {
            return Err(ZakuraValidationError::ControlNonceMismatch);
        }
        validate_initial_limits(self.accepted_limits, local)
            .map_err(ZakuraValidationError::from_wire_reject)?;
        if self.accepted_limits.max_frame_bytes > requested_limits.max_frame_bytes
            || self.accepted_limits.max_message_bytes > requested_limits.max_message_bytes
            || self.accepted_limits.max_open_streams > requested_limits.max_open_streams
            || self.accepted_limits.max_inbound_queue_depth
                > requested_limits.max_inbound_queue_depth
            || self.accepted_limits.idle_timeout_millis > requested_limits.idle_timeout_millis
        {
            return Err(ZakuraValidationError::ResourceLimit);
        }
        Ok(())
    }
}

/// Prelude written immediately after opening each Zakura stream.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct StreamPrelude {
    /// Fixed stream magic.
    pub magic: [u8; 4],
    /// Application stream kind.
    pub stream_kind: u16,
    /// Version of this stream kind.
    pub stream_version: u16,
    /// Optional request id.
    pub request_id: Option<u64>,
    /// Maximum frame bytes accepted by the stream opener.
    pub max_frame_bytes: u32,
}

impl StreamPrelude {
    /// Encodes this stream prelude.
    pub fn encode(&self) -> Result<Vec<u8>, ZakuraProtocolError> {
        let mut bytes = Vec::new();
        bytes.write_all(&self.magic)?;
        bytes.write_u16::<LittleEndian>(self.stream_kind)?;
        bytes.write_u16::<LittleEndian>(self.stream_version)?;
        match self.request_id {
            Some(request_id) => {
                bytes.write_u8(1)?;
                bytes.write_u64::<LittleEndian>(request_id)?;
            }
            None => bytes.write_u8(0)?,
        }
        bytes.write_u32::<LittleEndian>(self.max_frame_bytes)?;
        Ok(bytes)
    }

    /// Decodes a stream prelude.
    pub fn decode(bytes: &[u8]) -> Result<Self, ZakuraProtocolError> {
        let mut reader = Cursor::new(bytes);
        let mut magic = [0; 4];
        reader.read_exact(&mut magic)?;
        if magic != STREAM_PRELUDE_MAGIC {
            return Err(ZakuraProtocolError::InvalidMagic);
        }
        let stream_kind = reader.read_u16::<LittleEndian>()?;
        let stream_version = reader.read_u16::<LittleEndian>()?;
        let request_id = match reader.read_u8()? {
            0 => None,
            1 => Some(reader.read_u64::<LittleEndian>()?),
            value => return Err(ZakuraProtocolError::InvalidFlag(value)),
        };
        let max_frame_bytes = reader.read_u32::<LittleEndian>()?;
        reject_trailing(bytes, &reader)?;
        Ok(Self {
            magic,
            stream_kind,
            stream_version,
            request_id,
            max_frame_bytes,
        })
    }
}

/// A bounded Zakura stream frame.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Frame {
    /// Application message type.
    pub message_type: u16,
    /// Message flags.
    pub flags: u16,
    /// Bounded payload bytes.
    pub payload: Vec<u8>,
}

impl Frame {
    /// Encodes this frame if it fits in `max_frame_bytes`.
    pub fn encode(&self, max_frame_bytes: u32) -> Result<Vec<u8>, ZakuraProtocolError> {
        let max_frame_bytes = usize_from_u32(max_frame_bytes, "frame cap")?;
        if self.payload.len() > max_frame_bytes.saturating_sub(FRAME_HEADER_BYTES) {
            return Err(ZakuraProtocolError::OversizedPayload {
                actual: self.payload.len(),
                max: max_frame_bytes.saturating_sub(FRAME_HEADER_BYTES),
            });
        }
        let mut bytes = Vec::new();
        bytes.write_u16::<LittleEndian>(self.message_type)?;
        bytes.write_u16::<LittleEndian>(self.flags)?;
        bytes.write_u32::<LittleEndian>(u32_from_usize(self.payload.len(), "payload length")?)?;
        bytes.write_all(&self.payload)?;
        Ok(bytes)
    }

    /// Decodes this frame if it fits in `max_frame_bytes`.
    pub fn decode(bytes: &[u8], max_frame_bytes: u32) -> Result<Self, ZakuraProtocolError> {
        let max_frame_bytes = usize_from_u32(max_frame_bytes, "frame cap")?;
        if bytes.len() > max_frame_bytes {
            return Err(ZakuraProtocolError::OversizedPayload {
                actual: bytes.len(),
                max: max_frame_bytes,
            });
        }
        let mut reader = Cursor::new(bytes);
        let message_type = reader.read_u16::<LittleEndian>()?;
        let flags = reader.read_u16::<LittleEndian>()?;
        let payload_len = usize_from_u32(reader.read_u32::<LittleEndian>()?, "payload length")?;
        if payload_len > max_frame_bytes.saturating_sub(FRAME_HEADER_BYTES) {
            return Err(ZakuraProtocolError::OversizedPayload {
                actual: payload_len,
                max: max_frame_bytes.saturating_sub(FRAME_HEADER_BYTES),
            });
        }
        let payload = read_exact_vec(&mut reader, payload_len)?;
        reject_trailing(bytes, &reader)?;
        Ok(Self {
            message_type,
            flags,
            payload,
        })
    }
}

/// A pending inbound Iroh upgrade that must match a legacy prelude.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PendingUpgrade {
    /// Expected initiator peer id.
    pub expected_peer_id: ZakuraPeerId,
    /// Selected Zakura protocol.
    pub selected_zakura_protocol: u16,
    /// Initiator upgrade nonce.
    pub initiator_upgrade_nonce: [u8; 32],
    /// Responder upgrade nonce.
    pub responder_upgrade_nonce: [u8; 32],
    /// Legacy upgrade transcript hash.
    pub legacy_upgrade_transcript: [u8; 32],
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct PendingUpgradeEntry {
    pending: PendingUpgrade,
    expires_at: Instant,
}

/// A small bounded pending-upgrade registry keyed by authenticated Iroh id.
#[derive(Debug)]
pub struct PendingUpgradeRegistry {
    entries: HashMap<ZakuraPeerId, PendingUpgradeEntry>,
    max_entries: usize,
    ttl: Duration,
}

impl PendingUpgradeRegistry {
    /// Creates a bounded pending-upgrade registry.
    pub fn new(max_entries: usize, ttl: Duration) -> Self {
        Self {
            entries: HashMap::new(),
            max_entries,
            ttl,
        }
    }

    /// Inserts an expected pending upgrade.
    pub fn insert(
        &mut self,
        now: Instant,
        pending: PendingUpgrade,
    ) -> Result<(), ZakuraRejectReason> {
        self.prune_expired(now);
        if self.entries.len() >= self.max_entries
            && !self.entries.contains_key(&pending.expected_peer_id)
        {
            return Err(ZakuraRejectReason::ResourceLimit);
        }
        self.entries.insert(
            pending.expected_peer_id.clone(),
            PendingUpgradeEntry {
                pending,
                expires_at: now + self.ttl,
            },
        );
        Ok(())
    }

    /// Takes and consumes a matching pending upgrade, if one exists and has not expired.
    pub fn take(&mut self, now: Instant, peer_id: &ZakuraPeerId) -> Option<PendingUpgrade> {
        self.prune_expired(now);
        self.entries.remove(peer_id).map(|entry| entry.pending)
    }

    /// Returns the number of live pending entries.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Returns true if there are no live pending entries.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    fn prune_expired(&mut self, now: Instant) {
        self.entries.retain(|_, entry| entry.expires_at > now);
    }
}

/// A tiny authenticated-peer registry used by the Zakura supervisor.
#[derive(Debug, Default)]
pub struct ZakuraPeerSupervisor {
    peers: HashMap<ZakuraPeerId, [u8; TRANSCRIPT_HASH_BYTES]>,
}

impl ZakuraPeerSupervisor {
    /// Registers an authenticated peer or returns `Duplicate` without punishment.
    pub fn register_authenticated(
        &mut self,
        peer_id: ZakuraPeerId,
        transcript_hash: [u8; TRANSCRIPT_HASH_BYTES],
    ) -> crate::zakura::ZakuraUpgradeOutcome {
        if let Some(existing_hash) = self.peers.get(&peer_id) {
            // D6: the lexicographically smaller transcript hash wins; exact ties keep the incumbent.
            if existing_hash <= &transcript_hash {
                metrics::counter!("zakura.p2p.handshake.duplicate").increment(1);
                return crate::zakura::ZakuraUpgradeOutcome::Duplicate { peer_id };
            }
        }

        self.peers.insert(peer_id.clone(), transcript_hash);
        metrics::counter!("zakura.p2p.handshake.upgraded").increment(1);
        crate::zakura::ZakuraUpgradeOutcome::Upgraded { peer_id }
    }

    /// Removes an authenticated peer registration.
    pub fn deregister_authenticated(&mut self, peer_id: &ZakuraPeerId) {
        self.peers.remove(peer_id);
    }
}

impl PendingUpgrade {
    /// Creates a pending upgrade with an unset expiration.
    pub fn new(
        expected_peer_id: ZakuraPeerId,
        selected_zakura_protocol: u16,
        initiator_upgrade_nonce: [u8; 32],
        responder_upgrade_nonce: [u8; 32],
        legacy_upgrade_transcript: [u8; 32],
    ) -> Self {
        Self {
            expected_peer_id,
            selected_zakura_protocol,
            initiator_upgrade_nonce,
            responder_upgrade_nonce,
            legacy_upgrade_transcript,
        }
    }
}

/// Computes the canonical legacy upgrade transcript hash.
pub fn legacy_upgrade_transcript(
    initiator_version_message: &VersionMessage,
    responder_version_message: &VersionMessage,
    p2p_v2_upgrade_init: &P2pV2UpgradeInit,
    p2p_v2_upgrade_accept: &P2pV2UpgradeAccept,
) -> Result<[u8; TRANSCRIPT_HASH_BYTES], ZakuraProtocolError> {
    let mut state = Blake2bParams::new()
        .hash_length(TRANSCRIPT_HASH_BYTES)
        .personal(b"zakura-upgrade1")
        .to_state();

    write_version_message_to_hash(&mut state, initiator_version_message)?;
    write_version_message_to_hash(&mut state, responder_version_message)?;
    state.update(&P2pV2Upgrade::Init(p2p_v2_upgrade_init.clone()).encode()?);
    state.update(&P2pV2Upgrade::Accept(p2p_v2_upgrade_accept.clone()).encode()?);

    let hash = state.finalize();
    let mut out = [0; TRANSCRIPT_HASH_BYTES];
    out.copy_from_slice(hash.as_bytes());
    Ok(out)
}

/// Selects the highest shared Zakura protocol version.
pub fn select_zakura_protocol(
    local_min: u16,
    local_max: u16,
    remote_min: u16,
    remote_max: u16,
) -> Result<u16, ZakuraRejectReason> {
    if local_min > local_max || remote_min > remote_max {
        return Err(ZakuraRejectReason::IncompatibleZakuraProtocol);
    }
    let selected = min(local_max, remote_max);
    if selected < local_min || selected < remote_min {
        return Err(ZakuraRejectReason::IncompatibleZakuraProtocol);
    }
    Ok(selected)
}

/// An encoding or validation error in bounded Zakura protocol data.
#[derive(Error, Debug)]
pub enum ZakuraProtocolError {
    /// A payload exceeded its hard cap.
    #[error("Zakura payload length {actual} exceeds hard cap {max}")]
    OversizedPayload {
        /// Actual payload length.
        actual: usize,
        /// Maximum allowed payload length.
        max: usize,
    },

    /// An I/O error while encoding or decoding.
    #[error("Zakura wire I/O error: {0}")]
    Io(#[from] io::Error),

    /// The message type discriminator is unknown.
    #[error("invalid Zakura message discriminator {0}")]
    InvalidDiscriminator(u8),

    /// The network id is unknown.
    #[error("invalid Zakura network id {0}")]
    InvalidNetworkId(u32),

    /// The reject reason is unknown.
    #[error("invalid Zakura reject reason {0}")]
    InvalidRejectReason(u16),

    /// The control role is unknown.
    #[error("invalid Zakura control role {0}")]
    InvalidRole(u8),

    /// The control path is unknown.
    #[error("invalid Zakura handshake path {0}")]
    InvalidHandshakePath(u8),

    /// A boolean or option flag is invalid.
    #[error("invalid Zakura flag {0}")]
    InvalidFlag(u8),

    /// Magic bytes did not match the expected value.
    #[error("invalid Zakura magic")]
    InvalidMagic,

    /// A decoded payload had trailing bytes.
    #[error("trailing bytes in Zakura payload")]
    TrailingBytes,

    /// A bounded byte string was empty when an identity was required.
    #[error("empty Zakura {0}")]
    Empty(&'static str),

    /// A numeric conversion failed while handling bounded Zakura data.
    #[error("numeric overflow while encoding Zakura {0}")]
    NumericOverflow(&'static str),
}

struct PreludeStaticFields<'a> {
    magic: [u8; 8],
    prelude_version: u16,
    network_id: ZakuraNetworkId,
    chain_id: [u8; 32],
    capabilities: u64,
    iroh_node_id: &'a [u8],
    iroh_direct_addresses: &'a [Vec<u8>],
    iroh_relay_hint: Option<&'a [u8]>,
    max_control_frame_bytes: u32,
    max_open_streams: u16,
}

fn validate_prelude_static_fields(
    prelude: PreludeStaticFields<'_>,
    local: &ZakuraHandshakeConfig,
) -> Result<(), ZakuraRejectReason> {
    if prelude.magic != PRELUDE_MAGIC || prelude.prelude_version != local.prelude_version {
        return Err(ZakuraRejectReason::UnsupportedPreludeVersion);
    }
    if prelude.network_id != local.network_id {
        return Err(ZakuraRejectReason::WrongNetwork);
    }
    if prelude.chain_id != local.chain_id {
        return Err(ZakuraRejectReason::WrongChain);
    }
    if prelude.capabilities & local.required_capabilities != local.required_capabilities {
        return Err(ZakuraRejectReason::MissingRequiredCapability);
    }
    validate_peer_hints(
        prelude.iroh_node_id,
        prelude.iroh_direct_addresses,
        prelude.iroh_relay_hint,
    )
    .map_err(|_| ZakuraRejectReason::ResourceLimit)?;
    validate_resource_limits(
        prelude.max_control_frame_bytes,
        prelude.max_open_streams,
        local,
    )?;
    Ok(())
}

fn validate_peer_nonce_labels(
    peer_local_zebra_nonce: Nonce,
    peer_remote_zebra_nonce: Nonce,
    local_observed: ZakuraLegacyNonces,
) -> Result<(), ZakuraRejectReason> {
    if peer_local_zebra_nonce != local_observed.remote_zebra_nonce
        || peer_remote_zebra_nonce != local_observed.local_zebra_nonce
    {
        return Err(ZakuraRejectReason::TemporaryUnavailable);
    }
    Ok(())
}

fn validate_peer_hints(
    iroh_node_id: &[u8],
    iroh_direct_addresses: &[Vec<u8>],
    iroh_relay_hint: Option<&[u8]>,
) -> Result<(), ZakuraProtocolError> {
    validate_bounded_bytes(iroh_node_id, MAX_IROH_NODE_ID_BYTES, "iroh node id")?;
    if iroh_direct_addresses.len() > MAX_IROH_DIRECT_ADDRESSES {
        return Err(ZakuraProtocolError::OversizedPayload {
            actual: iroh_direct_addresses.len(),
            max: MAX_IROH_DIRECT_ADDRESSES,
        });
    }
    for address in iroh_direct_addresses {
        validate_bounded_bytes(address, MAX_IROH_DIRECT_ADDRESS_BYTES, "direct address")?;
    }
    if let Some(relay_hint) = iroh_relay_hint {
        validate_bounded_bytes(relay_hint, MAX_IROH_RELAY_HINT_BYTES, "relay hint")?;
    }
    Ok(())
}

fn validate_resource_limits(
    max_control_frame_bytes: u32,
    max_open_streams: u16,
    local: &ZakuraHandshakeConfig,
) -> Result<(), ZakuraRejectReason> {
    if max_control_frame_bytes == 0
        || max_control_frame_bytes > local.max_control_frame_bytes
        || max_open_streams == 0
        || max_open_streams > local.max_open_streams
    {
        return Err(ZakuraRejectReason::ResourceLimit);
    }
    Ok(())
}

fn validate_initial_limits(
    limits: ZakuraInitialLimits,
    local: &ZakuraHandshakeConfig,
) -> Result<(), ZakuraRejectReason> {
    // This negotiated ceiling is wider than most stream kinds need; per-kind
    // frame handling applies the effective cap before payload allocation.
    if limits.max_frame_bytes == 0
        || limits.max_frame_bytes > local.max_message_bytes
        || limits.max_message_bytes == 0
        || limits.max_message_bytes > local.max_message_bytes
        || limits.max_open_streams == 0
        || limits.max_open_streams > local.max_open_streams
        || limits.max_inbound_queue_depth == 0
        || limits.max_inbound_queue_depth > local.max_inbound_queue_depth
        || limits.idle_timeout_millis == 0
        || limits.idle_timeout_millis > local.max_idle_timeout_millis
    {
        return Err(ZakuraRejectReason::ResourceLimit);
    }
    Ok(())
}

impl ZakuraLimits {
    fn encode_to<W: Write>(&self, writer: &mut W) -> Result<(), ZakuraProtocolError> {
        writer.write_u32::<LittleEndian>(self.max_frame_bytes)?;
        writer.write_u32::<LittleEndian>(self.max_message_bytes)?;
        writer.write_u16::<LittleEndian>(self.max_open_streams)?;
        writer.write_u16::<LittleEndian>(self.max_inbound_queue_depth)?;
        writer.write_u32::<LittleEndian>(self.idle_timeout_millis)?;
        Ok(())
    }

    fn decode_from<R: Read>(reader: &mut R) -> Result<Self, ZakuraProtocolError> {
        Ok(Self {
            max_frame_bytes: reader.read_u32::<LittleEndian>()?,
            max_message_bytes: reader.read_u32::<LittleEndian>()?,
            max_open_streams: reader.read_u16::<LittleEndian>()?,
            max_inbound_queue_depth: reader.read_u16::<LittleEndian>()?,
            idle_timeout_millis: reader.read_u32::<LittleEndian>()?,
        })
    }
}

fn write_bounded_bytes<W: Write>(
    writer: &mut W,
    bytes: &[u8],
    max_len: usize,
) -> Result<(), ZakuraProtocolError> {
    validate_bounded_bytes(bytes, max_len, "byte string")?;
    writer.write_u16::<LittleEndian>(
        u16::try_from(bytes.len())
            .map_err(|_| ZakuraProtocolError::NumericOverflow("byte string length"))?,
    )?;
    writer.write_all(bytes)?;
    Ok(())
}

fn write_optional_bounded_bytes<W: Write>(
    writer: &mut W,
    bytes: Option<&[u8]>,
    max_len: usize,
) -> Result<(), ZakuraProtocolError> {
    match bytes {
        Some(bytes) => {
            writer.write_u8(1)?;
            write_bounded_bytes(writer, bytes, max_len)?;
        }
        None => writer.write_u8(0)?,
    }
    Ok(())
}

fn write_bounded_bytes_list<W: Write>(
    writer: &mut W,
    items: &[Vec<u8>],
    max_count: usize,
    max_item_len: usize,
) -> Result<(), ZakuraProtocolError> {
    if items.len() > max_count {
        return Err(ZakuraProtocolError::OversizedPayload {
            actual: items.len(),
            max: max_count,
        });
    }
    writer.write_u16::<LittleEndian>(
        u16::try_from(items.len())
            .map_err(|_| ZakuraProtocolError::NumericOverflow("list count"))?,
    )?;
    for item in items {
        write_bounded_bytes(writer, item, max_item_len)?;
    }
    Ok(())
}

fn read_bounded_bytes<R: Read>(
    reader: &mut R,
    max_len: usize,
) -> Result<Vec<u8>, ZakuraProtocolError> {
    let len = usize::from(reader.read_u16::<LittleEndian>()?);
    if len > max_len {
        return Err(ZakuraProtocolError::OversizedPayload {
            actual: len,
            max: max_len,
        });
    }
    read_exact_vec(reader, len)
}

fn read_optional_bounded_bytes<R: Read>(
    reader: &mut R,
    max_len: usize,
) -> Result<Option<Vec<u8>>, ZakuraProtocolError> {
    match reader.read_u8()? {
        0 => Ok(None),
        1 => Ok(Some(read_bounded_bytes(reader, max_len)?)),
        value => Err(ZakuraProtocolError::InvalidFlag(value)),
    }
}

fn read_bounded_bytes_list<R: Read>(
    reader: &mut R,
    max_count: usize,
    max_item_len: usize,
) -> Result<Vec<Vec<u8>>, ZakuraProtocolError> {
    let count = usize::from(reader.read_u16::<LittleEndian>()?);
    if count > max_count {
        return Err(ZakuraProtocolError::OversizedPayload {
            actual: count,
            max: max_count,
        });
    }
    let mut items = Vec::with_capacity(count);
    for _ in 0..count {
        items.push(read_bounded_bytes(reader, max_item_len)?);
    }
    Ok(items)
}

fn read_exact_vec<R: Read>(reader: &mut R, len: usize) -> Result<Vec<u8>, ZakuraProtocolError> {
    let mut bytes = vec![0; len];
    reader.read_exact(&mut bytes)?;
    Ok(bytes)
}

fn validate_bounded_bytes(
    bytes: &[u8],
    max_len: usize,
    field: &'static str,
) -> Result<(), ZakuraProtocolError> {
    if bytes.is_empty() {
        return Err(ZakuraProtocolError::Empty(field));
    }
    if bytes.len() > max_len {
        return Err(ZakuraProtocolError::OversizedPayload {
            actual: bytes.len(),
            max: max_len,
        });
    }
    Ok(())
}

fn reject_trailing(bytes: &[u8], reader: &Cursor<&[u8]>) -> Result<(), ZakuraProtocolError> {
    let consumed = usize::try_from(reader.position())
        .map_err(|_| ZakuraProtocolError::NumericOverflow("cursor position"))?;
    if consumed != bytes.len() {
        return Err(ZakuraProtocolError::TrailingBytes);
    }
    Ok(())
}

fn usize_from_u32(value: u32, field: &'static str) -> Result<usize, ZakuraProtocolError> {
    usize::try_from(value).map_err(|_| ZakuraProtocolError::NumericOverflow(field))
}

fn u32_from_usize(value: usize, field: &'static str) -> Result<u32, ZakuraProtocolError> {
    u32::try_from(value).map_err(|_| ZakuraProtocolError::NumericOverflow(field))
}

fn write_version_message_to_hash(
    state: &mut blake2b_simd::State,
    version: &VersionMessage,
) -> Result<(), ZakuraProtocolError> {
    let mut bytes = Vec::new();
    bytes.write_u32::<LittleEndian>(version.version.0)?;
    bytes.write_u64::<LittleEndian>(version.services.bits())?;
    bytes.write_i64::<LittleEndian>(version.timestamp.timestamp())?;
    version.address_recv.zcash_serialize(&mut bytes)?;
    version.address_from.zcash_serialize(&mut bytes)?;
    bytes.write_u64::<LittleEndian>(version.nonce.0)?;
    version.user_agent.zcash_serialize(&mut bytes)?;
    bytes.write_u32::<LittleEndian>(version.start_height.0)?;
    bytes.write_u8(u8::from(version.relay))?;
    state.update(&bytes);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::net::SocketAddr;

    use chrono::{TimeZone, Utc};
    use futures::{SinkExt, StreamExt};
    use proptest::prelude::*;
    use tokio_util::codec::{FramedRead, FramedWrite};

    use crate::protocol::external::{types::*, AddrInVersion, Codec, Message};

    fn local_config() -> ZakuraHandshakeConfig {
        ZakuraHandshakeConfig::for_network(&Network::Mainnet)
    }

    fn nonces() -> ZakuraLegacyNonces {
        ZakuraLegacyNonces {
            local_zebra_nonce: Nonce(10),
            remote_zebra_nonce: Nonce(20),
        }
    }

    fn init() -> P2pV2UpgradeInit {
        let local = local_config();
        P2pV2UpgradeInit {
            magic: PRELUDE_MAGIC,
            prelude_version: PRELUDE_VERSION,
            zakura_protocol_min: 1,
            zakura_protocol_max: 1,
            network_id: local.network_id,
            chain_id: local.chain_id,
            capabilities: 0,
            local_zebra_nonce: nonces().remote_zebra_nonce,
            remote_zebra_nonce: nonces().local_zebra_nonce,
            upgrade_nonce: [1; 32],
            iroh_node_id: vec![7; 32],
            iroh_direct_addresses: vec![b"127.0.0.1:0".to_vec()],
            iroh_relay_hint: None,
            max_control_frame_bytes: 1024,
            max_open_streams: 8,
        }
    }

    fn accept(init: &P2pV2UpgradeInit) -> P2pV2UpgradeAccept {
        let local = local_config();
        P2pV2UpgradeAccept {
            magic: PRELUDE_MAGIC,
            prelude_version: PRELUDE_VERSION,
            selected_zakura_protocol: 1,
            network_id: local.network_id,
            chain_id: local.chain_id,
            capabilities: 0,
            initiator_upgrade_nonce: init.upgrade_nonce,
            responder_upgrade_nonce: [2; 32],
            local_zebra_nonce: nonces().remote_zebra_nonce,
            remote_zebra_nonce: nonces().local_zebra_nonce,
            iroh_node_id: vec![8; 32],
            iroh_direct_addresses: vec![b"127.0.0.1:1".to_vec()],
            iroh_relay_hint: None,
            max_control_frame_bytes: 1024,
            max_open_streams: 8,
        }
    }

    #[test]
    fn prelude_roundtrip_and_rejects_trailing() {
        let upgrade = P2pV2Upgrade::Init(init());
        let encoded = upgrade.encode().expect("valid init encodes");
        assert_eq!(
            P2pV2Upgrade::decode(&encoded).expect("valid init decodes"),
            upgrade
        );

        let mut with_trailing = encoded;
        with_trailing.push(0);
        assert!(matches!(
            P2pV2Upgrade::decode(&with_trailing),
            Err(ZakuraProtocolError::TrailingBytes)
        ));
    }

    #[test]
    fn prelude_rejects_oversized_inputs_before_decoding() {
        let oversized = vec![0; MAX_PRELUDE_PAYLOAD_BYTES + 1];
        assert!(matches!(
            P2pV2Upgrade::decode(&oversized),
            Err(ZakuraProtocolError::OversizedPayload { .. })
        ));
    }

    #[test]
    fn init_validation_selects_overlap_and_rejects_bad_ranges() {
        let local = local_config();
        assert_eq!(init().validate(&local, nonces()), Ok(1));

        let mut bad = init();
        bad.zakura_protocol_min = 2;
        bad.zakura_protocol_max = 3;
        assert_eq!(
            bad.validate(&local, nonces()),
            Err(ZakuraRejectReason::IncompatibleZakuraProtocol)
        );
    }

    #[test]
    fn prelude_validation_rejects_network_chain_and_nonce_mismatch() {
        let local = local_config();

        let mut wrong_network = init();
        wrong_network.network_id = ZakuraNetworkId::Testnet;
        assert_eq!(
            wrong_network.validate(&local, nonces()),
            Err(ZakuraRejectReason::WrongNetwork)
        );

        let mut wrong_chain = init();
        wrong_chain.chain_id = [9; 32];
        assert_eq!(
            wrong_chain.validate(&local, nonces()),
            Err(ZakuraRejectReason::WrongChain)
        );

        let mut wrong_nonce = init();
        wrong_nonce.local_zebra_nonce = Nonce(99);
        assert_eq!(
            wrong_nonce.validate(&local, nonces()),
            Err(ZakuraRejectReason::TemporaryUnavailable)
        );
    }

    #[test]
    fn accept_validation_distinguishes_upgrade_nonce_mismatch() {
        let local = local_config();
        let init = init();
        let mut accept = accept(&init);
        accept.initiator_upgrade_nonce = [9; 32];

        assert_eq!(
            accept.validate(&local, nonces(), &init),
            Err(ZakuraValidationError::UpgradeNonceMismatch)
        );
        assert_eq!(
            accept
                .validate(&local, nonces(), &init)
                .unwrap_err()
                .failure_class(),
            ZakuraFailureClass::PotentiallyPunitive
        );
    }

    #[test]
    fn bounded_lists_enforce_maximum_lengths() {
        let mut too_many = init();
        too_many.iroh_direct_addresses = vec![b"x".to_vec(); MAX_IROH_DIRECT_ADDRESSES + 1];
        assert!(P2pV2Upgrade::Init(too_many).encode().is_err());

        let mut too_long = init();
        too_long.iroh_direct_addresses = vec![vec![1; MAX_IROH_DIRECT_ADDRESS_BYTES + 1]];
        assert!(P2pV2Upgrade::Init(too_long).encode().is_err());
    }

    proptest! {
        #[test]
        fn arbitrary_prelude_decode_never_panics(
            bytes in prop::collection::vec(any::<u8>(), 0..(MAX_PRELUDE_PAYLOAD_BYTES + 8))
        ) {
            let _ = P2pV2Upgrade::decode(&bytes);
        }

        #[test]
        fn arbitrary_control_hello_decode_never_panics(
            bytes in prop::collection::vec(any::<u8>(), 0..2048usize)
        ) {
            let _ = ZakuraControlHello::decode(&bytes);
        }

        #[test]
        fn arbitrary_control_ack_decode_never_panics(
            bytes in prop::collection::vec(any::<u8>(), 0..512usize)
        ) {
            let _ = ZakuraControlAck::decode(&bytes);
        }

        #[test]
        fn arbitrary_stream_prelude_decode_never_panics(
            bytes in prop::collection::vec(any::<u8>(), 0..64usize)
        ) {
            let _ = StreamPrelude::decode(&bytes);
        }

        #[test]
        fn arbitrary_frame_decode_never_panics(
            bytes in prop::collection::vec(any::<u8>(), 0..512usize),
            max_frame_bytes in 0u32..512
        ) {
            let _ = Frame::decode(&bytes, max_frame_bytes);
        }
    }

    #[test]
    fn control_hello_validates_identity_transcript_and_native_zeroes() {
        let local = local_config();
        let hello = ZakuraControlHello {
            magic: CONTROL_HELLO_MAGIC,
            control_version: CONTROL_VERSION,
            selected_zakura_protocol: 1,
            handshake_path: ZakuraHandshakePath::Upgraded,
            role: ZakuraControlRole::Initiator,
            network_id: local.network_id,
            chain_id: local.chain_id,
            iroh_node_id: vec![7; 32],
            peer_nonce: [3; 32],
            initiator_upgrade_nonce: [1; 32],
            responder_upgrade_nonce: [2; 32],
            legacy_upgrade_transcript: [4; 32],
            capabilities: 0,
            required_channels: 0,
            initial_limits: ZakuraInitialLimits {
                max_frame_bytes: 1024,
                max_message_bytes: 2048,
                max_open_streams: 8,
                max_inbound_queue_depth: 8,
                idle_timeout_millis: 1000,
            },
        };
        let expected = ZakuraControlValidation {
            local: &local,
            authenticated_remote_id: &[7; 32],
            selected_zakura_protocol: 1,
            handshake_path: ZakuraHandshakePath::Upgraded,
            remote_role: ZakuraControlRole::Initiator,
            initiator_upgrade_nonce: [1; 32],
            responder_upgrade_nonce: [2; 32],
            legacy_upgrade_transcript: [4; 32],
        };

        let encoded = hello.encode().expect("valid hello encodes");
        let decoded = ZakuraControlHello::decode(&encoded).expect("valid hello decodes");
        assert_eq!(decoded.validate(&expected), Ok(()));

        let mut wrong_identity = decoded.clone();
        wrong_identity.iroh_node_id = vec![8; 32];
        assert_eq!(
            wrong_identity.validate(&expected),
            Err(ZakuraValidationError::IdentityMismatch)
        );
        assert_eq!(
            wrong_identity
                .validate(&expected)
                .unwrap_err()
                .failure_class(),
            ZakuraFailureClass::PotentiallyPunitive
        );

        let mut wrong_transcript = decoded;
        wrong_transcript.legacy_upgrade_transcript = [5; 32];
        assert_eq!(
            wrong_transcript.validate(&expected),
            Err(ZakuraValidationError::TranscriptMismatch)
        );
        assert_eq!(
            wrong_transcript
                .validate(&expected)
                .unwrap_err()
                .failure_class(),
            ZakuraFailureClass::PotentiallyPunitive
        );

        let native = ZakuraControlHello {
            handshake_path: ZakuraHandshakePath::Native,
            initiator_upgrade_nonce: [0; 32],
            responder_upgrade_nonce: [0; 32],
            legacy_upgrade_transcript: [0; 32],
            ..hello
        };
        let native_expected = ZakuraControlValidation {
            handshake_path: ZakuraHandshakePath::Native,
            initiator_upgrade_nonce: [0; 32],
            responder_upgrade_nonce: [0; 32],
            legacy_upgrade_transcript: [0; 32],
            ..expected
        };
        assert_eq!(native.validate(&native_expected), Ok(()));
    }

    #[test]
    fn control_ack_echoes_exact_peer_nonces() {
        let ack = ZakuraControlAck {
            magic: CONTROL_ACK_MAGIC,
            control_version: CONTROL_VERSION,
            selected_zakura_protocol: 1,
            peer_nonce: [2; 32],
            remote_peer_nonce: [1; 32],
            accepted_capabilities: 0,
            accepted_channels: 0,
            accepted_limits: ZakuraAcceptedLimits {
                max_frame_bytes: 1024,
                max_message_bytes: 2048,
                max_open_streams: 8,
                max_inbound_queue_depth: 8,
                idle_timeout_millis: 1000,
            },
        };
        let requested_limits = ack.accepted_limits;
        let local = local_config();
        assert_eq!(
            ack.validate(1, [1; 32], [2; 32], &requested_limits, &local),
            Ok(())
        );
        assert_eq!(
            ack.validate(1, [9; 32], [2; 32], &requested_limits, &local),
            Err(ZakuraValidationError::ControlNonceMismatch)
        );

        let malicious_ack = ZakuraControlAck {
            accepted_limits: ZakuraAcceptedLimits {
                max_frame_bytes: requested_limits.max_frame_bytes + 1,
                ..requested_limits
            },
            ..ack
        };
        assert_eq!(
            malicious_ack.validate(1, [1; 32], [2; 32], &requested_limits, &local),
            Err(ZakuraValidationError::ResourceLimit)
        );

        // A malicious ack that over-caps any other field is rejected too.
        let over_cap_acks = [
            ZakuraAcceptedLimits {
                max_message_bytes: local.max_message_bytes + 1,
                ..requested_limits
            },
            ZakuraAcceptedLimits {
                max_open_streams: local.max_open_streams + 1,
                ..requested_limits
            },
            ZakuraAcceptedLimits {
                max_inbound_queue_depth: local.max_inbound_queue_depth + 1,
                ..requested_limits
            },
            ZakuraAcceptedLimits {
                idle_timeout_millis: local.max_idle_timeout_millis + 1,
                ..requested_limits
            },
        ];
        for accepted_limits in over_cap_acks {
            let over_cap_ack = ZakuraControlAck {
                accepted_limits,
                ..ack.clone()
            };
            assert_eq!(
                over_cap_ack.validate(1, [1; 32], [2; 32], &requested_limits, &local),
                Err(ZakuraValidationError::ResourceLimit)
            );
        }

        // A malicious ack with any zero/below-floor limit is rejected.
        let zero_acks = [
            ZakuraAcceptedLimits {
                max_frame_bytes: 0,
                ..requested_limits
            },
            ZakuraAcceptedLimits {
                max_message_bytes: 0,
                ..requested_limits
            },
            ZakuraAcceptedLimits {
                max_open_streams: 0,
                ..requested_limits
            },
            ZakuraAcceptedLimits {
                max_inbound_queue_depth: 0,
                ..requested_limits
            },
            ZakuraAcceptedLimits {
                idle_timeout_millis: 0,
                ..requested_limits
            },
        ];
        for accepted_limits in zero_acks {
            // Grant the full requested cap so the zero floor is the only failure.
            let requested = ZakuraInitialLimits {
                max_frame_bytes: local.max_control_frame_bytes,
                max_message_bytes: local.max_message_bytes,
                max_open_streams: local.max_open_streams,
                max_inbound_queue_depth: local.max_inbound_queue_depth,
                idle_timeout_millis: local.max_idle_timeout_millis,
            };
            let zero_ack = ZakuraControlAck {
                accepted_limits,
                ..ack.clone()
            };
            assert_eq!(
                zero_ack.validate(1, [1; 32], [2; 32], &requested, &local),
                Err(ZakuraValidationError::ResourceLimit)
            );
        }
    }

    #[test]
    fn initial_limits_allow_application_frames_above_control_cap() {
        let local = local_config();
        let limits = ZakuraInitialLimits {
            max_frame_bytes: local.max_message_bytes,
            max_message_bytes: local.max_message_bytes,
            max_open_streams: local.max_open_streams,
            max_inbound_queue_depth: local.max_inbound_queue_depth,
            idle_timeout_millis: local.max_idle_timeout_millis,
        };

        assert_eq!(validate_initial_limits(limits, &local), Ok(()));
        assert!(limits.max_frame_bytes > local.max_control_frame_bytes);
    }

    #[test]
    fn stream_prelude_and_frame_are_bounded() {
        let prelude = StreamPrelude {
            magic: STREAM_PRELUDE_MAGIC,
            stream_kind: 1,
            stream_version: 1,
            request_id: Some(10),
            max_frame_bytes: 16,
        };
        let encoded = prelude.encode().expect("stream prelude encodes");
        assert_eq!(StreamPrelude::decode(&encoded).unwrap(), prelude);

        let frame = Frame {
            message_type: 1,
            flags: 0,
            payload: vec![1; 8],
        };
        let encoded = frame.encode(16).expect("frame fits");
        assert_eq!(Frame::decode(&encoded, 16).unwrap(), frame);

        let oversized = Frame {
            payload: vec![1; 9],
            ..frame
        };
        assert!(oversized.encode(16).is_err());
    }

    #[test]
    fn pending_upgrade_registry_matches_and_expires_by_peer_id() {
        let now = Instant::now();
        let peer_id = ZakuraPeerId::new(vec![7; 32]).unwrap();
        let pending = PendingUpgrade::new(peer_id.clone(), 1, [1; 32], [2; 32], [3; 32]);
        let mut registry = PendingUpgradeRegistry::new(1, Duration::from_secs(1));

        registry.insert(now, pending).expect("under cap");
        assert_eq!(registry.len(), 1);
        assert!(registry
            .take(now + Duration::from_millis(500), &peer_id)
            .is_some());
        assert!(registry.is_empty());

        let pending = PendingUpgrade::new(peer_id.clone(), 1, [1; 32], [2; 32], [3; 32]);
        registry.insert(now, pending).expect("under cap");
        assert!(registry
            .take(now + Duration::from_secs(2), &peer_id)
            .is_none());
    }

    #[test]
    fn duplicate_supervisor_returns_duplicate_without_replacing_winner() {
        let peer_id = ZakuraPeerId::new(vec![7; 32]).unwrap();
        let mut supervisor = ZakuraPeerSupervisor::default();

        assert!(matches!(
            supervisor.register_authenticated(peer_id.clone(), [1; 32]),
            crate::zakura::ZakuraUpgradeOutcome::Upgraded { .. }
        ));
        assert!(matches!(
            supervisor.register_authenticated(peer_id.clone(), [2; 32]),
            crate::zakura::ZakuraUpgradeOutcome::Duplicate { .. }
        ));
        assert!(matches!(
            supervisor.register_authenticated(peer_id, [0; 32]),
            crate::zakura::ZakuraUpgradeOutcome::Upgraded { .. }
        ));
    }

    fn version(nonce: Nonce) -> VersionMessage {
        let addr: SocketAddr = "127.0.0.1:8233".parse().unwrap();
        VersionMessage {
            version: Version(1),
            services: PeerServices::NODE_NETWORK,
            timestamp: Utc::now(),
            address_recv: AddrInVersion::new(addr, PeerServices::NODE_NETWORK),
            address_from: AddrInVersion::new(addr, PeerServices::NODE_NETWORK),
            nonce,
            user_agent: "/Zebra:test/".to_string(),
            start_height: zebra_chain::block::Height(0),
            relay: true,
        }
    }

    fn frozen_version(services: PeerServices, user_agent: &str) -> VersionMessage {
        let addr: SocketAddr = "127.0.0.1:8233".parse().unwrap();

        VersionMessage {
            version: crate::constants::CURRENT_NETWORK_PROTOCOL_VERSION,
            services,
            timestamp: Utc
                .timestamp_opt(1_700_000_000, 0)
                .single()
                .expect("fixed timestamp is in range"),
            address_recv: AddrInVersion::new(addr, PeerServices::NODE_NETWORK),
            address_from: AddrInVersion::new(addr, services),
            nonce: Nonce(0x0102_0304_0506_0708),
            user_agent: user_agent.to_string(),
            start_height: zebra_chain::block::Height(1),
            relay: true,
        }
    }

    fn frozen_plain_zebra_version() -> VersionMessage {
        frozen_version(PeerServices::NODE_NETWORK, "/Zebra:compat/")
    }

    fn frozen_zakura_version() -> VersionMessage {
        frozen_version(
            PeerServices::NODE_NETWORK | PeerServices::NODE_P2P_V2,
            "/Zakura:7.0.0/Zebra:compat/",
        )
    }

    fn control_hello() -> ZakuraControlHello {
        let local = local_config();

        ZakuraControlHello {
            magic: CONTROL_HELLO_MAGIC,
            control_version: CONTROL_VERSION,
            selected_zakura_protocol: 1,
            handshake_path: ZakuraHandshakePath::Upgraded,
            role: ZakuraControlRole::Initiator,
            network_id: local.network_id,
            chain_id: local.chain_id,
            iroh_node_id: vec![7; 32],
            peer_nonce: [3; 32],
            initiator_upgrade_nonce: [1; 32],
            responder_upgrade_nonce: [2; 32],
            legacy_upgrade_transcript: [4; 32],
            capabilities: 0,
            required_channels: 0,
            initial_limits: ZakuraInitialLimits {
                max_frame_bytes: 1024,
                max_message_bytes: 2048,
                max_open_streams: 8,
                max_inbound_queue_depth: 8,
                idle_timeout_millis: 1000,
            },
        }
    }

    fn control_ack() -> ZakuraControlAck {
        ZakuraControlAck {
            magic: CONTROL_ACK_MAGIC,
            control_version: CONTROL_VERSION,
            selected_zakura_protocol: 1,
            peer_nonce: [2; 32],
            remote_peer_nonce: [1; 32],
            accepted_capabilities: 0,
            accepted_channels: 0,
            accepted_limits: ZakuraAcceptedLimits {
                max_frame_bytes: 1024,
                max_message_bytes: 2048,
                max_open_streams: 8,
                max_inbound_queue_depth: 8,
                idle_timeout_millis: 1000,
            },
        }
    }

    fn stream_prelude() -> StreamPrelude {
        StreamPrelude {
            magic: STREAM_PRELUDE_MAGIC,
            stream_kind: 1,
            stream_version: 1,
            request_id: Some(10),
            max_frame_bytes: 1024,
        }
    }

    fn frame_sample() -> Frame {
        Frame {
            message_type: 1,
            flags: 0,
            payload: b"zakura-compat-v1".to_vec(),
        }
    }

    fn encode_version_message(version: VersionMessage) -> Vec<u8> {
        let (rt, _init_guard) = zebra_test::init_async();

        rt.block_on(async {
            let mut bytes = Vec::new();
            {
                let mut writer = FramedWrite::new(&mut bytes, Codec::builder().finish());
                writer
                    .send(Message::Version(version))
                    .await
                    .expect("frozen version message serializes");
            }
            bytes
        })
    }

    fn decode_version_message(bytes: &[u8]) -> VersionMessage {
        decode_version_message_result(bytes).expect("frozen version vector decodes")
    }

    #[derive(Clone, Debug)]
    enum WireMessage {
        Version(VersionMessage),
        P2pV2Upgrade(P2pV2Upgrade),
        ControlHello(ZakuraControlHello),
        ControlAck(ZakuraControlAck),
        StreamPrelude(StreamPrelude),
        Frame { value: Frame, max_frame_bytes: u32 },
    }

    impl WireMessage {
        fn encode(&self) -> Vec<u8> {
            match self {
                Self::Version(value) => encode_version_message(value.clone()),
                Self::P2pV2Upgrade(value) => value.encode().expect("valid message encodes"),
                Self::ControlHello(value) => value.encode().expect("valid message encodes"),
                Self::ControlAck(value) => value.encode().expect("valid message encodes"),
                Self::StreamPrelude(value) => value.encode().expect("valid message encodes"),
                Self::Frame {
                    value,
                    max_frame_bytes,
                } => value
                    .encode(*max_frame_bytes)
                    .expect("valid message encodes"),
            }
        }

        fn assert_decodes(&self, bytes: &[u8]) {
            match self {
                Self::Version(value) => assert_eq!(decode_version_message(bytes), *value),
                Self::P2pV2Upgrade(value) => {
                    assert_eq!(P2pV2Upgrade::decode(bytes).unwrap(), *value)
                }
                Self::ControlHello(value) => {
                    assert_eq!(ZakuraControlHello::decode(bytes).unwrap(), *value)
                }
                Self::ControlAck(value) => {
                    assert_eq!(ZakuraControlAck::decode(bytes).unwrap(), *value)
                }
                Self::StreamPrelude(value) => {
                    assert_eq!(StreamPrelude::decode(bytes).unwrap(), *value)
                }
                Self::Frame {
                    value,
                    max_frame_bytes,
                } => assert_eq!(Frame::decode(bytes, *max_frame_bytes).unwrap(), *value),
            }
        }

        fn assert_rejects_trailing_bytes(&self) {
            let mut bytes = self.encode();
            bytes.extend_from_slice(&[0xaa, 0xbb, 0xcc]);

            match self {
                Self::Version(_) => {}
                Self::P2pV2Upgrade(_) => assert!(
                    P2pV2Upgrade::decode(&bytes).is_err(),
                    "p2pv2up accepted trailing bytes",
                ),
                Self::ControlHello(_) => assert!(
                    ZakuraControlHello::decode(&bytes).is_err(),
                    "control hello accepted trailing bytes",
                ),
                Self::ControlAck(_) => assert!(
                    ZakuraControlAck::decode(&bytes).is_err(),
                    "control ack accepted trailing bytes",
                ),
                Self::StreamPrelude(_) => assert!(
                    StreamPrelude::decode(&bytes).is_err(),
                    "stream prelude accepted trailing bytes",
                ),
                Self::Frame {
                    max_frame_bytes, ..
                } => assert!(
                    Frame::decode(&bytes, *max_frame_bytes).is_err(),
                    "frame accepted trailing bytes",
                ),
            }
        }
    }

    fn decode_version_message_result(bytes: &[u8]) -> Result<VersionMessage, crate::BoxError> {
        let (rt, _init_guard) = zebra_test::init_async();

        rt.block_on(async {
            let mut reader = FramedRead::new(Cursor::new(bytes), Codec::builder().finish());
            match reader
                .next()
                .await
                .ok_or_else(|| -> crate::BoxError { "no message decoded".into() })??
            {
                Message::Version(version) => Ok(version),
                message => Err(format!("unexpected wire message: {message:?}").into()),
            }
        })
    }

    fn wire_messages() -> Vec<WireMessage> {
        let init = init();
        vec![
            WireMessage::Version(frozen_plain_zebra_version()),
            WireMessage::Version(frozen_zakura_version()),
            WireMessage::P2pV2Upgrade(P2pV2Upgrade::Init(init.clone())),
            WireMessage::P2pV2Upgrade(P2pV2Upgrade::Accept(accept(&init))),
            WireMessage::P2pV2Upgrade(P2pV2Upgrade::Reject(P2pV2UpgradeReject {
                magic: PRELUDE_MAGIC,
                prelude_version: PRELUDE_VERSION,
                reason: ZakuraRejectReason::IncompatibleZakuraProtocol,
            })),
            WireMessage::ControlHello(control_hello()),
            WireMessage::ControlAck(control_ack()),
            WireMessage::StreamPrelude(stream_prelude()),
            WireMessage::Frame {
                value: frame_sample(),
                max_frame_bytes: 1024,
            },
        ]
    }

    #[test]
    fn transcript_hash_binds_preludes() {
        let init = init();
        let accept = accept(&init);
        let hash =
            legacy_upgrade_transcript(&version(Nonce(1)), &version(Nonce(2)), &init, &accept)
                .expect("valid transcript hashes");

        let mut tampered_accept = accept;
        tampered_accept.capabilities = 1;
        let tampered_hash = legacy_upgrade_transcript(
            &version(Nonce(1)),
            &version(Nonce(2)),
            &init,
            &tampered_accept,
        )
        .expect("valid tampered transcript hashes");

        assert_ne!(hash, tampered_hash);
    }

    #[test]
    fn compat_i1_i2_legacy_version_messages_roundtrip() {
        let expected = frozen_plain_zebra_version();
        let bytes = encode_version_message(expected.clone());

        assert_eq!(decode_version_message(&bytes), expected);
        assert!(!expected.services.contains(PeerServices::NODE_P2P_V2));
        assert!(!expected
            .address_from
            .untrusted_services()
            .contains(PeerServices::NODE_P2P_V2));

        let expected = frozen_zakura_version();
        let bytes = encode_version_message(expected.clone());

        assert_eq!(decode_version_message(&bytes), expected);
        assert!(expected.services.contains(PeerServices::NODE_P2P_V2));
        assert!(expected
            .address_from
            .untrusted_services()
            .contains(PeerServices::NODE_P2P_V2));
        assert!(!expected
            .address_recv
            .untrusted_services()
            .contains(PeerServices::NODE_P2P_V2));
    }

    #[test]
    fn compat_i5_p2pv2up_wire_messages_roundtrip() {
        for message in wire_messages()
            .into_iter()
            .filter(|message| matches!(message, WireMessage::P2pV2Upgrade(_)))
        {
            let bytes = message.encode();
            message.assert_decodes(&bytes);
        }
    }

    #[test]
    fn compat_i5_control_and_stream_messages_roundtrip() {
        for message in wire_messages().into_iter().filter(|message| {
            matches!(
                message,
                WireMessage::ControlHello(_)
                    | WireMessage::ControlAck(_)
                    | WireMessage::StreamPrelude(_)
                    | WireMessage::Frame { .. }
            )
        }) {
            let bytes = message.encode();
            message.assert_decodes(&bytes);
        }
    }

    #[test]
    fn compat_i3_v1_zakura_decoders_reject_unknown_trailing_data() {
        for message in wire_messages()
            .into_iter()
            .filter(|message| !matches!(message, WireMessage::Version(_)))
        {
            message.assert_rejects_trailing_bytes();
        }
    }

    #[test]
    fn compat_i3_unknown_service_bits_are_truncated_without_selecting_zakura() {
        let unknown_high_bit = 1 << 63;
        let services =
            PeerServices::from_bits_truncate(PeerServices::NODE_NETWORK.bits() | unknown_high_bit);

        assert_eq!(services, PeerServices::NODE_NETWORK);
        assert!(!services.contains(PeerServices::NODE_P2P_V2));
    }
}
