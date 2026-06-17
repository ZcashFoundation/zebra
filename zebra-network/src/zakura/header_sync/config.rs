use super::{error::*, validation::*, wire::*, *};
use crate::zakura::ServicePeerLimits;

/// Header-sync peer status advertisement.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct HeaderSyncStatus {
    /// Sender's best known tip height.
    pub tip_height: block::Height,
    /// Sender's best known tip hash.
    pub tip_hash: block::Hash,
    /// Sender's lowest contiguous header height.
    pub anchor_height: block::Height,
    /// Maximum headers the sender will serve per response.
    pub max_headers_per_response: u32,
    /// Maximum concurrent `GetHeaders` requests the sender will service.
    pub max_inflight_requests: u16,
}

impl HeaderSyncStatus {
    pub(super) fn encode_to<W: Write>(&self, writer: &mut W) -> Result<(), HeaderSyncWireError> {
        write_height(writer, self.tip_height)?;
        self.tip_hash.zcash_serialize(&mut *writer)?;
        write_height(writer, self.anchor_height)?;
        writer.write_u32::<LittleEndian>(clamp_advertised_range(self.max_headers_per_response))?;
        writer.write_u16::<LittleEndian>(self.max_inflight_requests)?;
        Ok(())
    }

    pub(super) fn decode_from<R: Read>(reader: &mut R) -> Result<Self, HeaderSyncWireError> {
        Ok(Self {
            tip_height: read_height(reader)?,
            tip_hash: block::Hash::zcash_deserialize(&mut *reader)?,
            anchor_height: read_height(reader)?,
            max_headers_per_response: clamp_advertised_range(reader.read_u32::<LittleEndian>()?),
            max_inflight_requests: reader.read_u16::<LittleEndian>()?,
        })
    }
}

impl Default for HeaderSyncStatus {
    fn default() -> Self {
        Self {
            tip_height: block::Height::MIN,
            tip_hash: block::Hash([0; 32]),
            anchor_height: block::Height::MIN,
            max_headers_per_response: DEFAULT_HS_RANGE,
            max_inflight_requests: DEFAULT_HS_MAX_INFLIGHT,
        }
    }
}

/// Header-sync configuration nested under the Zakura P2P-v2 config.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct ZakuraHeaderSyncConfig {
    /// Maximum headers this node advertises per `GetHeaders` response.
    pub max_headers_per_response: u32,
    /// Maximum concurrent `GetHeaders` requests this node advertises per peer.
    pub max_inflight_requests: u16,
    /// How often this node sends unsolicited status refreshes after local frontier changes.
    #[serde(with = "humantime_serde")]
    pub status_refresh_interval: Duration,
    /// Header-sync peer caps and queue limits owned by this reactor.
    pub peer_limits: ServicePeerLimits,
    /// Accept full blocks delivered on Zakura full-block gossip paths.
    ///
    /// Disabling this keeps range-based header sync and legacy request/response
    /// active while forcing block bodies to arrive through the block-sync stream.
    pub accept_new_blocks: bool,
    /// Optional trusted header-sync anchor height.
    ///
    /// When unset, header sync starts from genesis. When set, [`anchor_hash`](Self::anchor_hash)
    /// must also be set and must match genesis or a configured checkpoint.
    pub anchor_height: Option<block::Height>,
    /// Optional trusted header-sync anchor hash.
    ///
    /// When unset, header sync starts from genesis. When set, [`anchor_height`](Self::anchor_height)
    /// must also be set and must match genesis or a configured checkpoint.
    pub anchor_hash: Option<block::Hash>,
}

impl Default for ZakuraHeaderSyncConfig {
    fn default() -> Self {
        Self {
            max_headers_per_response: DEFAULT_HS_RANGE,
            max_inflight_requests: DEFAULT_HS_MAX_INFLIGHT,
            status_refresh_interval: DEFAULT_HS_STATUS_REFRESH_INTERVAL,
            peer_limits: ServicePeerLimits::default(),
            accept_new_blocks: true,
            anchor_height: None,
            anchor_hash: None,
        }
    }
}

impl ZakuraHeaderSyncConfig {
    /// Return the clamped served-range advertisement for wire status messages.
    pub fn advertised_max_headers_per_response(&self) -> u32 {
        clamp_advertised_range(self.max_headers_per_response)
    }

    /// Return the locally capped in-flight advertisement for status messages.
    pub fn advertised_max_inflight_requests(&self) -> u16 {
        self.max_inflight_requests
            .clamp(1, LOCAL_MAX_HS_INFLIGHT_PER_PEER)
    }

    /// Return the configured trusted anchor, or genesis when no override is configured.
    pub fn anchor(
        &self,
        network: &Network,
    ) -> Result<(block::Height, block::Hash), HeaderSyncStartError> {
        match (self.anchor_height, self.anchor_hash) {
            (Some(height), Some(hash)) => Ok((height, hash)),
            (None, None) => Ok((block::Height(0), network.genesis_hash())),
            _ => Err(HeaderSyncStartError::IncompleteAnchor),
        }
    }
}

/// Returns the serialized byte length of a stream-5 header on `network`.
pub fn header_sync_header_bytes_for_network(network: &Network) -> usize {
    if network
        .parameters()
        .is_some_and(|parameters| parameters.is_regtest())
    {
        REGTEST_HEADER_BYTES
    } else {
        COMMON_HEADER_BYTES
    }
}

/// Maximum `Headers` count that fits both the stream-5 payload cap and the app frame cap.
pub fn header_sync_count_by_byte_budget(network: &Network, max_frame_bytes: u32) -> u32 {
    let frame_payload_cap = usize::try_from(max_frame_bytes)
        .unwrap_or(usize::MAX)
        .saturating_sub(FRAME_HEADER_BYTES);
    let payload_cap = MAX_HS_MESSAGE_BYTES.min(frame_payload_cap);
    let header_bytes =
        header_sync_header_bytes_for_network(network).saturating_add(HEADER_SYNC_BODY_SIZE_BYTES);
    let count = payload_cap
        .saturating_sub(HEADER_SYNC_MESSAGE_TYPE_BYTES + HEADER_SYNC_COUNT_BYTES)
        / header_bytes;

    u32::try_from(count)
        .unwrap_or(u32::MAX)
        .clamp(1, MAX_HS_RANGE)
}

/// Clamp an outbound `GetHeaders.count` by peer, hard, payload, and frame caps.
pub fn clamp_header_sync_request_count(
    desired_count: u32,
    peer_max_headers_per_response: u32,
    network: &Network,
    max_frame_bytes: u32,
) -> u32 {
    desired_count
        .min(clamp_advertised_range(peer_max_headers_per_response))
        .min(MAX_HS_RANGE)
        .min(header_sync_count_by_byte_budget(network, max_frame_bytes))
        .max(1)
}

/// Maximum inbound `GetHeaders.count` this node will serve.
pub fn inbound_get_headers_count_limit(
    config: &ZakuraHeaderSyncConfig,
    network: &Network,
    max_frame_bytes: u32,
) -> u32 {
    clamp_header_sync_request_count(
        u32::MAX,
        config.advertised_max_headers_per_response(),
        network,
        max_frame_bytes,
    )
}

/// Truncate a served header run so the encoded `Headers` response fits the byte budgets.
pub fn truncate_headers_to_byte_budget(
    mut headers: Vec<Arc<block::Header>>,
    network: &Network,
    max_frame_bytes: u32,
) -> Vec<Arc<block::Header>> {
    let max_count = usize::try_from(header_sync_count_by_byte_budget(network, max_frame_bytes))
        .expect("header-sync byte-budget count fits in usize");
    headers.truncate(max_count);
    headers
}
