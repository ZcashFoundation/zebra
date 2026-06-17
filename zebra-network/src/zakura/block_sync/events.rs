use super::{scheduler::*, state::*, wire::*, *};

/// Committed header metadata used by block sync to schedule and validate a body.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct BlockSyncBlockMeta {
    /// Header-known block height whose body is missing.
    pub height: block::Height,
    /// Committed header hash expected from the downloaded body.
    pub hash: block::Hash,
    /// Advisory or confirmed body-size estimate for scheduling.
    pub size: BlockSizeEstimate,
}

/// Facts accepted by the block-sync scaffold and later reactor.
#[derive(Clone, Debug)]
pub enum BlockSyncEvent {
    /// A peer became available for stream-6 block sync.
    PeerConnected(BlockSyncPeerSession),
    /// A peer disconnected; all of its outstanding work is dropped.
    PeerDisconnected(ZakuraPeerId),
    /// Inbound stream-6 message from `peer`.
    WireMessage {
        /// Serving peer.
        peer: ZakuraPeerId,
        /// Decoded stream-6 message.
        msg: BlockSyncMessage,
    },
    /// Stream-6 frame decoding failed after handler admission.
    WireDecodeFailed {
        /// Peer that sent the malformed frame.
        peer: ZakuraPeerId,
        /// Decode/validation error.
        error: Arc<BlockSyncWireError>,
    },
    /// Header sync advanced the committed header target.
    HeaderTipChanged {
        /// Current best header height.
        height: block::Height,
        /// Current best header hash.
        hash: block::Hash,
    },
    /// State finalized or verified-body frontiers changed.
    StateFrontiersChanged(BlockSyncFrontiers),
    /// State grew the verified body chain tip.
    ChainTipGrow(BlockSyncFrontiers),
    /// State reset the verified body chain tip after a rollback or best-chain switch.
    ChainTipReset(BlockSyncFrontiers),
    /// Driver returned the current body-missing, header-known heights with committed hashes.
    NeededBlocks(Vec<BlockSyncBlockMeta>),
    /// Node wiring finished or abandoned a `Block` response to an inbound `GetBlocks`.
    BlockRangeResponseFinished {
        /// Peer whose served-response slot can be released.
        peer: ZakuraPeerId,
        /// First requested height.
        start_height: block::Height,
        /// Requested block count.
        requested_count: u32,
        /// Number of blocks read from state and sent in the response.
        returned_count: u32,
    },
    /// State returned committed bodies requested by a peer and the reactor should send them.
    BlockRangeResponseReady {
        /// Peer whose inbound request is being served.
        peer: ZakuraPeerId,
        /// First requested height.
        start_height: block::Height,
        /// Requested block count.
        requested_count: u32,
        /// Bounded committed blocks returned by state.
        blocks: Vec<(block::Height, Arc<block::Block>, usize)>,
    },
}

/// Actions emitted by the future block-sync reactor for the service seam.
#[derive(Clone, Debug)]
pub enum BlockSyncAction {
    /// Queue a typed stream-6 message to a peer.
    SendMessage {
        /// Destination peer.
        peer: ZakuraPeerId,
        /// Message that should be written to the peer's stream.
        msg: BlockSyncMessage,
    },
    /// Ask node wiring to read `missing_block_bodies`, header hashes, and size hints.
    QueryNeededBlocks {
        /// Current verified body tip.
        verified_block_tip: block::Height,
        /// Current best header target.
        best_header_tip: block::Height,
    },
    /// Ask node wiring to read committed bodies for an inbound `GetBlocks`.
    QueryBlocksByHeightRange {
        /// Peer that requested the range.
        peer: ZakuraPeerId,
        /// First height.
        start: block::Height,
        /// Maximum count.
        count: u32,
    },
    /// Parent-first body ready for B3's verifier/commit driver.
    SubmitBlock {
        /// Block body that is contiguous above `verified_block_tip`.
        block: Arc<block::Block>,
    },
    /// Report peer misbehavior to the supervisor.
    Misbehavior {
        /// Misbehaving peer.
        peer: ZakuraPeerId,
        /// Reason for reporting.
        reason: BlockSyncMisbehavior,
    },
}

/// Block-sync peer-accounting violations.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum BlockSyncMisbehavior {
    /// A stream-6 payload was malformed before semantic handling.
    MalformedMessage,
    /// A peer sent blocks that were not requested.
    UnsolicitedBlock,
    /// A peer requested more blocks than this node advertised it can serve.
    GetBlocksTooLong,
    /// A peer exceeded this node's inbound `GetBlocks` serving budget.
    GetBlocksSpam,
    /// A peer supplied a body whose hash or size does not match committed metadata.
    InvalidBlock,
    /// A peer supplied a body outside the tolerated scheduling-size deviation.
    SizeMismatch,
    /// Peer status is internally impossible.
    InvalidStatus,
    /// A response terminator arrived without an outstanding range.
    UnsolicitedDone,
    /// A peer reported a requested range unavailable.
    RangeUnavailable,
    /// A peer sent too many status frames.
    StatusSpam,
}
