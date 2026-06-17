use super::{config::*, error::*, validation::*, wire::*, *};

/// Cached state frontiers used by the header-sync reactor.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct HeaderSyncFrontiers {
    /// Shared finalized height `F`, supplied by state.
    pub finalized_height: block::Height,
    /// Highest verified block body height, supplied by state.
    pub verified_block_tip: block::Height,
}

/// Startup inputs for the dependency-neutral header-sync reactor.
#[derive(Clone, Debug)]
pub struct HeaderSyncStartup {
    /// Active network.
    pub network: Network,
    /// Trusted anchor height and hash.
    pub anchor: (block::Height, block::Hash),
    /// Cached state frontiers at startup.
    pub frontiers: HeaderSyncFrontiers,
    /// Durable best header tip loaded from storage at startup.
    pub best_header_tip: Option<(block::Height, block::Hash)>,
    /// Local stream-5 advertisement.
    pub config: ZakuraHeaderSyncConfig,
    /// Negotiated or local application frame cap for header-sync responses.
    pub max_frame_bytes: u32,
    /// Per-request timeout.
    pub request_timeout: Duration,
    /// Minimum interval between unsolicited status refreshes to a peer.
    pub status_refresh_interval: Duration,
    /// Optional JSONL trace emitter for header-sync runtime events.
    pub trace: ZakuraTrace,
    /// Shared shutdown signal owned by the embedding endpoint or test harness.
    pub shutdown: CancellationToken,
    /// Enables outbound range scheduling and state-backed header actions.
    pub range_state_actions_enabled: bool,
    /// Enables relaying inbound `NewBlock` messages after local block acceptance is wired.
    pub inbound_new_block_acceptance_enabled: bool,
}

impl HeaderSyncStartup {
    /// Build a startup config from the active network and durable/frontier facts.
    pub fn new(
        network: Network,
        anchor: (block::Height, block::Hash),
        frontiers: HeaderSyncFrontiers,
        best_header_tip: Option<(block::Height, block::Hash)>,
        config: ZakuraHeaderSyncConfig,
        max_frame_bytes: u32,
    ) -> Self {
        Self {
            network,
            anchor,
            frontiers,
            best_header_tip,
            config,
            max_frame_bytes,
            request_timeout: DEFAULT_HS_REQUEST_TIMEOUT,
            status_refresh_interval: DEFAULT_HS_STATUS_REFRESH_INTERVAL,
            trace: ZakuraTrace::noop(),
            shutdown: CancellationToken::new(),
            range_state_actions_enabled: false,
            inbound_new_block_acceptance_enabled: false,
        }
    }
}

/// Cheap cloneable handle used by other services to inform header sync.
#[derive(Clone, Debug)]
pub struct HeaderSyncHandle {
    pub(super) events: mpsc::Sender<HeaderSyncEvent>,
    pub(super) lifecycle: mpsc::UnboundedSender<HeaderSyncEvent>,
    pub(super) tip: watch::Receiver<(block::Height, block::Hash)>,
}

impl HeaderSyncHandle {
    /// Send a fact/event to the header-sync reactor.
    pub async fn send(
        &self,
        event: HeaderSyncEvent,
    ) -> Result<(), mpsc::error::SendError<HeaderSyncEvent>> {
        self.events.send(event).await
    }

    /// Try to send a fact/event without awaiting.
    pub fn try_send(
        &self,
        event: HeaderSyncEvent,
    ) -> Result<(), mpsc::error::TrySendError<HeaderSyncEvent>> {
        self.events.try_send(event)
    }

    /// Send a peer lifecycle event without sharing the bounded wire-event queue.
    pub fn send_lifecycle(
        &self,
        event: HeaderSyncEvent,
    ) -> Result<(), mpsc::error::SendError<HeaderSyncEvent>> {
        self.lifecycle
            .send(event)
            .map_err(|error| mpsc::error::SendError(error.0))
    }

    /// Subscribe to best-header frontier updates.
    pub fn subscribe_tip(&self) -> watch::Receiver<(block::Height, block::Hash)> {
        self.tip.clone()
    }

    /// Return the currently cached best-header frontier.
    pub fn best_header_tip(&self) -> (block::Height, block::Hash) {
        *self.tip.borrow()
    }
}

/// Facts accepted by the header-sync reactor.
#[derive(Clone, Debug)]
pub enum HeaderSyncEvent {
    /// A peer became available for stream-5 header sync.
    PeerConnected(ZakuraPeerId),
    /// A peer disconnected; all of its outstanding work is dropped.
    PeerDisconnected(ZakuraPeerId),
    /// State committed a full block.
    FullBlockCommitted {
        /// Committed block height.
        height: block::Height,
        /// Committed block hash.
        hash: block::Hash,
        /// Committed block header. Transient only; not retained by runtime state.
        header: Arc<block::Header>,
    },
    /// The node's block pipeline accepted an inbound `NewBlock` body.
    NewBlockAccepted {
        /// Source peer.
        peer: ZakuraPeerId,
        /// Accepted block height.
        height: block::Height,
        /// Accepted block hash.
        hash: block::Hash,
        /// Accepted full block.
        block: Arc<block::Block>,
    },
    /// The node's block pipeline reported an inbound `NewBlock` was already known.
    NewBlockDuplicate {
        /// Source peer.
        peer: ZakuraPeerId,
        /// Duplicate block height.
        height: block::Height,
        /// Duplicate block hash.
        hash: block::Hash,
    },
    /// The node's block pipeline rejected an inbound `NewBlock` body.
    NewBlockRejected {
        /// Source peer.
        peer: ZakuraPeerId,
        /// Rejected block hash.
        hash: block::Hash,
    },
    /// Inbound stream-5 message from `peer`.
    WireMessage {
        /// Serving peer.
        peer: ZakuraPeerId,
        /// Decoded stream-5 message.
        msg: HeaderSyncMessage,
    },
    /// Inbound stream-5 frame from `peer` whose decode depends on reactor state.
    WireFrame {
        /// Serving peer.
        peer: ZakuraPeerId,
        /// Raw stream-5 frame.
        frame: Frame,
    },
    /// Stream-5 frame decoding failed after handler admission.
    WireDecodeFailed {
        /// Peer that sent the malformed frame.
        peer: ZakuraPeerId,
        /// Decode/validation error.
        error: Arc<HeaderSyncWireError>,
    },
    /// State finalized or verified-body frontiers changed.
    StateFrontiersChanged(HeaderSyncFrontiers),
    /// State successfully committed a header range.
    HeaderRangeCommitted {
        /// First committed height.
        start_height: block::Height,
        /// New best header tip height.
        tip_height: block::Height,
        /// New best header tip hash.
        tip_hash: block::Hash,
    },
    /// State rejected a previously requested range.
    HeaderRangeCommitFailed {
        /// Peer that supplied the failed range.
        peer: ZakuraPeerId,
        /// First failed range height.
        start_height: block::Height,
        /// Failed range count.
        count: u32,
        /// Whether state rejected peer data or hit a local resource/channel failure.
        kind: HeaderSyncCommitFailureKind,
    },
    /// Node wiring finished or abandoned a `Headers` response to an inbound `GetHeaders`.
    HeaderRangeResponseFinished {
        /// Peer whose served-response slot can be released.
        peer: ZakuraPeerId,
        /// First requested height.
        start_height: block::Height,
        /// Requested header count.
        requested_count: u32,
        /// Number of headers read from state and sent in the response.
        returned_count: u32,
    },
}

/// Actions emitted by the header-sync reactor for the eventual node wiring.
#[derive(Clone, Debug)]
pub enum HeaderSyncAction {
    /// Send a stream-5 message to a peer.
    SendMessage {
        /// Destination peer.
        peer: ZakuraPeerId,
        /// Message to send.
        msg: HeaderSyncMessage,
    },
    /// Ask state to commit a contiguous header range.
    CommitHeaderRange {
        /// Peer that supplied the range.
        peer: ZakuraPeerId,
        /// Parent anchor hash for the first header.
        anchor: block::Hash,
        /// First header height.
        start_height: block::Height,
        /// Headers to commit. This is an output payload, not reactor state.
        headers: Vec<Arc<block::Header>>,
        /// Whether the range is expected to be finalized by checkpoint policy.
        finalized: bool,
    },
    /// Ask state for the durable best header tip.
    QueryBestHeaderTip,
    /// Ask state for a bounded contiguous range of headers.
    QueryHeadersByHeightRange {
        /// Peer that requested the range.
        peer: ZakuraPeerId,
        /// First height.
        start: block::Height,
        /// Maximum count.
        count: u32,
    },
    /// Ask state for missing block-body gaps.
    QueryMissingBlockBodies {
        /// First height to consider.
        from: block::Height,
        /// Maximum number of heights.
        limit: u32,
    },
    /// Report peer misbehavior to the supervisor.
    Misbehavior {
        /// Misbehaving peer.
        peer: ZakuraPeerId,
        /// Reason for reporting.
        reason: HeaderSyncMisbehavior,
    },
    /// Notify body download wiring that header-known body gaps exist.
    BodyGaps {
        /// First missing height.
        from: block::Height,
        /// Last missing height.
        to: block::Height,
    },
    /// Forward an unseen valid full tip block to one eligible stream-5 peer.
    ForwardNewBlock {
        /// Source peer, if the block was received from the network.
        source: Option<ZakuraPeerId>,
        /// Destination peer.
        peer: ZakuraPeerId,
        /// Block height from the coinbase transaction.
        height: block::Height,
        /// Block hash used for deduplication.
        hash: block::Hash,
        /// Full block to forward.
        block: Arc<block::Block>,
    },
    /// Inform later block-pipeline wiring that a validated tip block arrived.
    NewBlockReceived {
        /// Source peer.
        peer: ZakuraPeerId,
        /// Block height from the coinbase transaction.
        height: block::Height,
        /// Block hash used for deduplication.
        hash: block::Hash,
        /// Full block received from the peer.
        block: Arc<block::Block>,
    },
}

/// Header-sync peer-accounting violations.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum HeaderSyncMisbehavior {
    /// Peer status is internally impossible.
    InvalidStatus,
    /// `Headers` arrived without an outstanding request.
    UnsolicitedHeaders,
    /// `Headers` was empty and made no progress.
    EmptyHeaders,
    /// `Headers` exceeded the outstanding request contract.
    ResponseTooLong,
    /// Peer supplied a range that failed state/contextual commit.
    InvalidRange,
    /// A stream-5 payload was malformed before semantic handling.
    MalformedMessage,
    /// A peer sent semantic `Status` messages faster than the v1 budget.
    StatusSpam,
    /// A peer sent semantic `NewBlock` messages faster than the v1 budget.
    NewBlockSpam,
    /// A peer exceeded this node's inbound `GetHeaders` serving budget.
    GetHeadersSpam,
    /// A peer requested more headers than this node advertised it can serve.
    GetHeadersTooLong,
    /// A stream-5 message came from a peer with no active header-sync state.
    UnknownPeer,
    /// A full-block tip flood failed stateless validation.
    InvalidNewBlock,
}

/// State commit failure classification returned to the reactor by node wiring.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum HeaderSyncCommitFailureKind {
    /// The supplied headers failed contextual validation or checkpoint consistency.
    InvalidPeerRange,
    /// Local storage/channel/resource failure; do not score the peer.
    Local,
}

/// A single outbound `GetHeaders` range used to validate the next response.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct HeaderSyncRequestContract {
    /// First requested height.
    pub start_height: block::Height,
    /// Requested header count.
    pub count: u32,
}

impl HeaderSyncRequestContract {
    /// Create a bounded request contract.
    pub fn new(start_height: block::Height, count: u32) -> Result<Self, HeaderSyncWireError> {
        validate_get_headers_count(count)?;
        Ok(Self {
            start_height,
            count,
        })
    }
}
