use super::{error::*, wire::*, *};

/// Default number of blocks advertised per response.
///
/// Set to the wire ceiling so block-body requests batch as many blocks as the
/// per-response byte cap ([`MAX_BS_RESPONSE_BYTES`]) allows. Small early-chain
/// blocks ride in large counts; large near-tip blocks are byte-capped to fewer.
pub const DEFAULT_BS_BLOCKS_PER_RESPONSE: u32 = 128;
/// Default number of in-flight block requests advertised per peer.
///
/// Combined with the global byte budget ([`DEFAULT_BS_MAX_INFLIGHT_BLOCK_BYTES`])
/// this gives each peer enough work to keep its stream busy while avoiding a
/// response-frame backlog that can outlive the connection.
pub const DEFAULT_BS_MAX_INFLIGHT: u16 = 16;
/// Default expected serving-peer fanout used to derive the per-peer in-flight
/// byte cap (`max_inflight_block_bytes / expected_peers`).
///
/// Bounds how much of the global byte budget a single peer can reserve so one
/// fast peer cannot starve the others once the slot cap stops binding. `0`
/// disables per-peer byte fairness.
pub const DEFAULT_BS_EXPECTED_PEERS: usize = 4;
/// Default total response byte target advertised per range response.
pub const DEFAULT_BS_MAX_RESPONSE_BYTES: u32 = 32 * 1024 * 1024;
/// Default global byte budget reserved for later block-download scheduling.
pub const DEFAULT_BS_MAX_INFLIGHT_BLOCK_BYTES: u64 = 4 * 1024 * 1024 * 1024;
/// Default maximum submitted block applies awaiting verifier completion.
///
/// The checkpoint verifier resolves a checkpoint window only after the whole
/// window is queued, so this defaults to one maximum checkpoint gap.
pub const DEFAULT_BS_MAX_SUBMITTED_BLOCK_APPLIES: usize =
    zebra_chain::parameters::checkpoint::constants::MAX_CHECKPOINT_HEIGHT_GAP;
/// Default block-sync request timeout.
pub const DEFAULT_BS_REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
/// Default block-sync status refresh interval reserved for later advertisement.
pub const DEFAULT_BS_STATUS_REFRESH_INTERVAL: Duration = Duration::from_secs(30);
/// Default tolerated size-hint deviation percentage reserved for later soft scoring.
pub const DEFAULT_BS_SIZE_DEVIATION_TOLERANCE: u32 = 200;
/// Default block-sync peer fanout for the same requested range.
pub const DEFAULT_BS_FANOUT: usize = 2;
/// Default body lag where block sync pauses new downloads and lets block propagation finish.
pub const DEFAULT_BS_NEAR_TIP_BODY_DOWNLOAD_PAUSE_BLOCKS: u32 = 2;
/// Maximum peer-advertised aggregate byte target accepted per requested range.
///
/// A range response is sent as one `Block` frame per body, and each body frame
/// remains independently bounded by `MAX_BS_MESSAGE_BYTES`. This aggregate cap
/// only controls how many bounded body frames a server sends before `BlocksDone`.
pub const MAX_BS_RESPONSE_BYTES: u32 = DEFAULT_BS_MAX_RESPONSE_BYTES;

/// Block-sync peer status advertisement.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct BlockSyncStatus {
    /// Earliest block body this peer can serve.
    pub servable_low: block::Height,
    /// Highest contiguous verified block body this peer can serve.
    pub servable_high: block::Height,
    /// Hash of `servable_high`.
    pub tip_hash: block::Hash,
    /// Maximum blocks the sender will serve per requested range.
    pub max_blocks_per_response: u32,
    /// Maximum concurrent `GetBlocks` requests the sender will service.
    pub max_inflight_requests: u16,
    /// Maximum total response bytes the sender targets per requested range.
    pub max_response_bytes: u32,
}

impl BlockSyncStatus {
    pub(super) fn encode_to<W: Write>(&self, writer: &mut W) -> Result<(), BlockSyncWireError> {
        write_height(writer, self.servable_low)?;
        write_height(writer, self.servable_high)?;
        self.tip_hash.zcash_serialize(&mut *writer)?;
        writer.write_u32::<LittleEndian>(clamp_advertised_blocks(self.max_blocks_per_response))?;
        writer.write_u16::<LittleEndian>(self.max_inflight_requests)?;
        writer.write_u32::<LittleEndian>(self.max_response_bytes.max(1))?;
        Ok(())
    }

    pub(super) fn decode_from<R: Read>(reader: &mut R) -> Result<Self, BlockSyncWireError> {
        Ok(Self {
            servable_low: read_height(reader)?,
            servable_high: read_height(reader)?,
            tip_hash: block::Hash::zcash_deserialize(&mut *reader)?,
            max_blocks_per_response: clamp_advertised_blocks(reader.read_u32::<LittleEndian>()?),
            max_inflight_requests: clamp_advertised_inflight(reader.read_u16::<LittleEndian>()?),
            max_response_bytes: clamp_advertised_response_bytes(reader.read_u32::<LittleEndian>()?),
        })
    }
}

impl Default for BlockSyncStatus {
    fn default() -> Self {
        Self {
            servable_low: block::Height::MIN,
            servable_high: block::Height::MIN,
            tip_hash: block::Hash([0; 32]),
            max_blocks_per_response: DEFAULT_BS_BLOCKS_PER_RESPONSE,
            max_inflight_requests: DEFAULT_BS_MAX_INFLIGHT,
            max_response_bytes: DEFAULT_BS_MAX_RESPONSE_BYTES,
        }
    }
}

/// Block-sync configuration nested under the Zakura P2P-v2 config.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct ZakuraBlockSyncConfig {
    /// Deprecated compatibility key for older rollout configs.
    ///
    /// Zakura block sync is now selected by the top-level `v2_p2p` flag. This
    /// field is accepted but ignored so older configs keep parsing.
    #[doc(hidden)]
    #[serde(
        default,
        skip_serializing,
        deserialize_with = "deserialize_ignored_replace_legacy_syncer"
    )]
    pub replace_legacy_syncer: bool,
    /// Maximum blocks this node advertises per `GetBlocks` response.
    pub max_blocks_per_response: u32,
    /// Maximum concurrent `GetBlocks` requests this node advertises per peer.
    pub max_inflight_requests: u16,
    /// Maximum total response bytes this node advertises per `GetBlocks` response.
    pub max_response_bytes: u32,
    /// Maximum estimated bytes reserved for in-flight and buffered block bodies.
    pub max_inflight_block_bytes: u64,
    /// Maximum block bodies submitted to the verifier before completed applies
    /// release more submission slots.
    pub max_submitted_block_applies: usize,
    /// Expected serving-peer fanout used to derive the per-peer in-flight byte cap
    /// (`max_inflight_block_bytes / expected_peers`).
    ///
    /// Prevents one fast peer from reserving the whole byte budget once the
    /// per-peer slot cap stops binding. `0` disables per-peer byte fairness.
    pub expected_peers: usize,
    /// Timeout for an outstanding block-body range request.
    #[serde(with = "humantime_serde")]
    pub request_timeout: Duration,
    /// How often this node sends unsolicited status refreshes after local frontier changes.
    #[serde(with = "humantime_serde")]
    pub status_refresh_interval: Duration,
    /// Percentage deviation from advertised body-size hints tolerated before soft scoring.
    pub size_deviation_tolerance: u32,
    /// Number of peers later range scheduling may fan out to for the same body gap.
    pub fanout: usize,
    /// Body lag at or below which block sync pauses new downloads near the header tip.
    pub near_tip_body_download_pause_blocks: u32,
    /// Block-sync peer caps and queue limits owned by this service.
    pub peer_limits: ServicePeerLimits,
}

fn deserialize_ignored_replace_legacy_syncer<'de, D>(deserializer: D) -> Result<bool, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let _ = bool::deserialize(deserializer)?;
    Ok(false)
}

impl Default for ZakuraBlockSyncConfig {
    fn default() -> Self {
        Self {
            replace_legacy_syncer: false,
            max_blocks_per_response: DEFAULT_BS_BLOCKS_PER_RESPONSE,
            max_inflight_requests: DEFAULT_BS_MAX_INFLIGHT,
            max_response_bytes: DEFAULT_BS_MAX_RESPONSE_BYTES,
            max_inflight_block_bytes: DEFAULT_BS_MAX_INFLIGHT_BLOCK_BYTES,
            max_submitted_block_applies: DEFAULT_BS_MAX_SUBMITTED_BLOCK_APPLIES,
            expected_peers: DEFAULT_BS_EXPECTED_PEERS,
            request_timeout: DEFAULT_BS_REQUEST_TIMEOUT,
            status_refresh_interval: DEFAULT_BS_STATUS_REFRESH_INTERVAL,
            size_deviation_tolerance: DEFAULT_BS_SIZE_DEVIATION_TOLERANCE,
            fanout: DEFAULT_BS_FANOUT,
            near_tip_body_download_pause_blocks: DEFAULT_BS_NEAR_TIP_BODY_DOWNLOAD_PAUSE_BLOCKS,
            peer_limits: ServicePeerLimits::default(),
        }
    }
}

impl ZakuraBlockSyncConfig {
    /// Return the clamped block-count advertisement for wire status messages.
    pub fn advertised_max_blocks_per_response(&self) -> u32 {
        clamp_advertised_blocks(self.max_blocks_per_response)
    }

    /// Return the locally capped in-flight advertisement for status messages.
    pub fn advertised_max_inflight_requests(&self) -> u16 {
        clamp_advertised_inflight(self.max_inflight_requests)
    }

    /// Return the non-zero response byte advertisement for status messages.
    pub fn advertised_max_response_bytes(&self) -> u32 {
        clamp_advertised_response_bytes(self.max_response_bytes)
    }

    /// Return the non-zero verifier submission cap.
    pub fn submitted_apply_limit(&self) -> usize {
        self.max_submitted_block_applies.max(1)
    }

    /// Per-peer in-flight byte cap derived from the global budget and expected fanout.
    ///
    /// Floored at one advertised response so a peer can always reserve at least a
    /// full response (otherwise a tiny budget divided by `expected_peers` would
    /// starve every peer); the fair-share cap only binds above that. Returns
    /// `u64::MAX` (no per-peer limit) when `expected_peers` is `0`.
    pub fn per_peer_byte_cap(&self) -> u64 {
        match self.expected_peers {
            0 => u64::MAX,
            peers => (self.max_inflight_block_bytes / peers as u64)
                .max(u64::from(self.advertised_max_response_bytes())),
        }
    }

    /// Build the inert local status used before the block-sync reactor is wired.
    pub fn initial_status(&self) -> BlockSyncStatus {
        BlockSyncStatus {
            max_blocks_per_response: self.advertised_max_blocks_per_response(),
            max_inflight_requests: self.advertised_max_inflight_requests(),
            max_response_bytes: self.advertised_max_response_bytes(),
            ..BlockSyncStatus::default()
        }
    }
}

/// Clamp an advertised block count to the hard stream-6 request cap.
pub fn clamp_advertised_blocks(count: u32) -> u32 {
    count.clamp(1, MAX_BS_BLOCKS_PER_REQUEST)
}

/// Clamp an advertised in-flight request count to the local status ceiling.
pub fn clamp_advertised_inflight(count: u16) -> u16 {
    count.clamp(1, DEFAULT_BS_MAX_INFLIGHT)
}

/// Clamp an advertised response byte target to the largest stream-6 message.
pub fn clamp_advertised_response_bytes(bytes: u32) -> u32 {
    bytes.clamp(1, MAX_BS_RESPONSE_BYTES)
}

/// Maximum inbound `GetBlocks.count` this node will serve before looking at body sizes.
pub fn inbound_get_blocks_count_limit(config: &ZakuraBlockSyncConfig) -> u32 {
    config
        .advertised_max_blocks_per_response()
        .clamp(1, MAX_BS_BLOCKS_PER_REQUEST)
}
