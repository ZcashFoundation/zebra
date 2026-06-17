use super::{error::*, events::*, wire::*, *};

pub(super) fn validate_anchor(
    network: &Network,
    anchor: (block::Height, block::Hash),
) -> Result<(), HeaderSyncStartError> {
    let expected = if anchor.0 == block::Height(0) {
        Some(network.genesis_hash())
    } else {
        network.checkpoint_list().hash(anchor.0)
    };
    match expected {
        Some(hash) if hash == anchor.1 => Ok(()),
        _ => Err(HeaderSyncStartError::InvalidAnchor { anchor }),
    }
}

pub(super) fn next_height(height: block::Height) -> Option<block::Height> {
    height.0.checked_add(1).map(block::Height)
}

pub(super) fn previous_height(height: block::Height) -> Option<block::Height> {
    height.0.checked_sub(1).map(block::Height)
}

pub(super) fn height_after_count(start: block::Height, count: u32) -> Option<block::Height> {
    start.0.checked_add(count).map(block::Height)
}

pub(super) fn count_between(start: block::Height, end: block::Height) -> u32 {
    end.0
        .checked_sub(start.0)
        .and_then(|diff| diff.checked_add(1))
        .unwrap_or(0)
}

pub(super) fn insert_peer(
    row: &mut serde_json::Map<String, Value>,
    key: &'static str,
    peer: &ZakuraPeerId,
) {
    row.insert(key.to_string(), Value::String(trace_peer_label(peer)));
}

pub(super) fn insert_height(
    row: &mut serde_json::Map<String, Value>,
    key: &'static str,
    height: block::Height,
) {
    insert_u64(row, key, u64::from(height.0));
}

pub(super) fn insert_hash(
    row: &mut serde_json::Map<String, Value>,
    key: &'static str,
    hash: block::Hash,
) {
    row.insert(key.to_string(), Value::String(format!("{hash}")));
}

pub(super) fn insert_u64(row: &mut serde_json::Map<String, Value>, key: &'static str, value: u64) {
    row.insert(key.to_string(), Value::Number(Number::from(value)));
}

pub(super) fn insert_optional_str(
    row: &mut serde_json::Map<String, Value>,
    key: &'static str,
    value: Option<&'static str>,
) {
    row.insert(
        key.to_string(),
        value.map_or(Value::Null, |value| Value::String(value.to_string())),
    );
}

pub(super) fn misbehavior_reason_label(reason: HeaderSyncMisbehavior) -> &'static str {
    match reason {
        HeaderSyncMisbehavior::InvalidStatus => "invalid_status",
        HeaderSyncMisbehavior::UnsolicitedHeaders => "unsolicited_headers",
        HeaderSyncMisbehavior::EmptyHeaders => "empty_headers",
        HeaderSyncMisbehavior::ResponseTooLong => "response_too_long",
        HeaderSyncMisbehavior::InvalidRange => "invalid_range",
        HeaderSyncMisbehavior::MalformedMessage => "malformed_message",
        HeaderSyncMisbehavior::StatusSpam => "status_spam",
        HeaderSyncMisbehavior::NewBlockSpam => "new_block_spam",
        HeaderSyncMisbehavior::GetHeadersSpam => "get_headers_spam",
        HeaderSyncMisbehavior::GetHeadersTooLong => "get_headers_too_long",
        HeaderSyncMisbehavior::UnknownPeer => "unknown_peer",
        HeaderSyncMisbehavior::InvalidNewBlock => "invalid_new_block",
    }
}

pub(super) fn commit_failure_reason_label(kind: HeaderSyncCommitFailureKind) -> &'static str {
    match kind {
        HeaderSyncCommitFailureKind::InvalidPeerRange => "invalid_peer_range",
        HeaderSyncCommitFailureKind::Local => "local",
    }
}

/// Peer/request bounds required to decode a header response without over-allocation.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct HeaderSyncDecodeContext {
    /// The matching in-flight request, when a `Headers` response is expected.
    pub requested: Option<ExpectedHeadersResponse>,
    /// Peer's advertised response cap.
    pub peer_max_headers_per_response: u32,
}

impl HeaderSyncDecodeContext {
    /// Context for messages that are not `Headers` responses.
    pub fn control() -> Self {
        Self {
            requested: None,
            peer_max_headers_per_response: DEFAULT_HS_RANGE,
        }
    }

    /// Context for a `Headers` response to `requested`.
    pub fn for_headers_response(
        requested: ExpectedHeadersResponse,
        peer_max_headers_per_response: u32,
    ) -> Self {
        Self {
            requested: Some(requested),
            peer_max_headers_per_response: clamp_advertised_range(peer_max_headers_per_response),
        }
    }

    pub(super) fn headers_response_limit(self) -> Result<Option<usize>, HeaderSyncWireError> {
        let Some(requested) = self.requested else {
            return Ok(None);
        };
        let cap = min(
            min(requested.count, self.peer_max_headers_per_response),
            MAX_HS_RANGE,
        );
        Some(usize_from_u32(cap, "headers response limit")).transpose()
    }
}

/// Stateless header validation context.
#[derive(Copy, Clone, Debug)]
pub struct HeaderSyncValidationContext<'a> {
    /// Active network, used for Equihash solution-size policy.
    pub network: &'a Network,
    /// Wall clock used for future-time checks.
    pub now: DateTime<Utc>,
    /// First height in the received run.
    pub start_height: block::Height,
    /// Matching request and peer cap for count validation.
    pub decode_context: HeaderSyncDecodeContext,
}

/// Run all context-free validation checks for an inbound `Headers` response.
#[tracing::instrument(skip(headers, context))]
pub async fn validate_headers_stateless(
    headers: Vec<Arc<block::Header>>,
    context: HeaderSyncValidationContext<'_>,
) -> Result<(), HeaderSyncWireError> {
    validate_header_count(headers.len(), context.decode_context)?;
    validate_internal_continuity(&headers)?;
    validate_header_times(&headers, context.now, context.start_height)?;
    validate_solution_sizes(&headers, context.network)?;
    validate_pow_spawn_blocking(headers, context.network).await
}

/// Check that a header range links to its anchor and is internally contiguous.
pub fn validate_header_range_links(
    anchor: block::Hash,
    headers: &[Arc<block::Header>],
) -> Result<(), HeaderSyncWireError> {
    let Some(first) = headers.first() else {
        return Ok(());
    };

    if first.previous_block_hash != anchor {
        return Err(HeaderSyncWireError::FirstHeaderDoesNotLink);
    }

    validate_internal_continuity(headers)
}

/// Run all context-free validation checks for an inbound full-block tip flood.
#[tracing::instrument(skip(block, network))]
pub async fn validate_new_block_stateless(
    block: Arc<block::Block>,
    network: &Network,
    now: DateTime<Utc>,
    height: block::Height,
) -> Result<(), HeaderSyncWireError> {
    let header = block.header.clone();
    validate_header_times(std::slice::from_ref(&header), now, height)?;
    validate_solution_sizes(std::slice::from_ref(&header), network)?;
    validate_pow_spawn_blocking(vec![header], network).await
}

pub(super) fn validate_header_count(
    len: usize,
    context: HeaderSyncDecodeContext,
) -> Result<(), HeaderSyncWireError> {
    let Some(max_headers) = context.headers_response_limit()? else {
        return Err(HeaderSyncWireError::UnsolicitedHeaders);
    };
    validate_headers_len(len, max_headers)
}

pub(super) fn validate_internal_continuity(
    headers: &[Arc<block::Header>],
) -> Result<(), HeaderSyncWireError> {
    for adjacent in headers.windows(2) {
        let previous_hash = block::Hash::from(adjacent[0].as_ref());
        if previous_hash != adjacent[1].previous_block_hash {
            return Err(HeaderSyncWireError::NonContiguousHeaders);
        }
    }
    Ok(())
}

pub(super) fn validate_header_times(
    headers: &[Arc<block::Header>],
    now: DateTime<Utc>,
    start_height: block::Height,
) -> Result<(), HeaderSyncWireError> {
    for (offset, header) in headers.iter().enumerate() {
        let offset = u32::try_from(offset)
            .map_err(|_| HeaderSyncWireError::NumericOverflow("header height offset"))?;
        let height = block::Height(
            start_height
                .0
                .checked_add(offset)
                .ok_or(HeaderSyncWireError::NumericOverflow("header height"))?,
        );
        let hash = block::Hash::from(header.as_ref());
        header.time_is_valid_at(now, &height, &hash)?;
    }
    Ok(())
}

pub(super) fn validate_solution_sizes(
    headers: &[Arc<block::Header>],
    network: &Network,
) -> Result<(), HeaderSyncWireError> {
    let expect_regtest = network
        .parameters()
        .is_some_and(|parameters| parameters.is_regtest());
    for header in headers {
        match (expect_regtest, header.solution) {
            (true, equihash::Solution::Regtest(_))
            | (true, equihash::Solution::Common(_))
            | (false, equihash::Solution::Common(_)) => {}
            _ => return Err(HeaderSyncWireError::WrongEquihashSolutionSize),
        }
    }
    Ok(())
}

pub(super) async fn validate_pow_spawn_blocking(
    headers: Vec<Arc<block::Header>>,
    network: &Network,
) -> Result<(), HeaderSyncWireError> {
    let skip_pow_filter = network
        .parameters()
        .is_some_and(|parameters| parameters.is_regtest());
    tokio::task::spawn_blocking(move || validate_pow_blocking(&headers, skip_pow_filter)).await?
}

pub(super) fn validate_pow_blocking(
    headers: &[Arc<block::Header>],
    skip_pow_filter: bool,
) -> Result<(), HeaderSyncWireError> {
    if skip_pow_filter {
        return Ok(());
    }

    for header in headers {
        header.solution.check(header)?;
        let hash = block::Hash::from(header.as_ref());
        validate_difficulty_filter(hash, header.difficulty_threshold)?;
    }
    Ok(())
}

pub(super) fn validate_difficulty_filter(
    hash: block::Hash,
    difficulty_threshold: CompactDifficulty,
) -> Result<(), HeaderSyncWireError> {
    let threshold = difficulty_threshold
        .to_expanded()
        .ok_or(HeaderSyncWireError::InvalidDifficultyThreshold)?;
    if hash > threshold {
        return Err(HeaderSyncWireError::DifficultyFilter { hash, threshold });
    }
    Ok(())
}

pub(super) fn validate_get_headers_count(count: u32) -> Result<(), HeaderSyncWireError> {
    if count == 0 {
        return Err(HeaderSyncWireError::ZeroHeaderRequestCount);
    }
    if count > MAX_HS_RANGE {
        return Err(HeaderSyncWireError::HeaderCountLimit {
            actual: usize_from_u32(count, "headers count")?,
            max: usize_from_u32(MAX_HS_RANGE, "headers cap")?,
        });
    }
    Ok(())
}

pub(super) fn validate_headers_len(len: usize, max: usize) -> Result<(), HeaderSyncWireError> {
    if len > max {
        return Err(HeaderSyncWireError::HeaderCountLimit { actual: len, max });
    }
    Ok(())
}

pub(super) fn validate_body_sizes_len(
    headers: usize,
    body_sizes: usize,
) -> Result<(), HeaderSyncWireError> {
    if headers != body_sizes {
        return Err(HeaderSyncWireError::BodySizeCountMismatch {
            headers,
            body_sizes,
        });
    }
    Ok(())
}

pub(super) fn clamp_advertised_range(value: u32) -> u32 {
    value.clamp(1, MAX_HS_RANGE)
}

pub(super) fn write_height<W: Write>(
    writer: &mut W,
    height: block::Height,
) -> Result<(), HeaderSyncWireError> {
    writer.write_u32::<LittleEndian>(height.0)?;
    Ok(())
}

pub(super) fn read_height<R: Read>(reader: &mut R) -> Result<block::Height, HeaderSyncWireError> {
    let height = block::Height(reader.read_u32::<LittleEndian>()?);
    if height > block::Height::MAX {
        return Err(HeaderSyncWireError::HeightOutOfRange(height.0));
    }
    Ok(height)
}

pub(super) fn reject_trailing(
    bytes: &[u8],
    reader: &Cursor<&[u8]>,
) -> Result<(), HeaderSyncWireError> {
    let consumed = usize::try_from(reader.position())
        .map_err(|_| HeaderSyncWireError::NumericOverflow("cursor position"))?;
    if consumed != bytes.len() {
        return Err(HeaderSyncWireError::TrailingBytes);
    }
    Ok(())
}

pub(super) fn usize_from_u32(
    value: u32,
    field: &'static str,
) -> Result<usize, HeaderSyncWireError> {
    usize::try_from(value).map_err(|_| HeaderSyncWireError::NumericOverflow(field))
}

pub(super) fn u32_from_usize(
    value: usize,
    field: &'static str,
) -> Result<u32, HeaderSyncWireError> {
    u32::try_from(value).map_err(|_| HeaderSyncWireError::NumericOverflow(field))
}
