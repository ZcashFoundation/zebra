//! The verify-and-commit tower service for the known-hash IBD engine.
//!
//! Verification is a tower middleware on top of the Buffer'd state service
//! (design doc §4.3): each [`VerifyAndCommit`] call runs the pure [`convert`]
//! step on the global rayon pool through [`zebra_consensus`]'s
//! [`spawn_fifo`] bridge, then commits the resulting
//! [`CheckpointVerifiedBlock`] to the state. The returned future resolves
//! only after the state accepts the block, so conversions run in parallel
//! across the rayon pool while the commit completions remain the engine's
//! only progress signal.
//!
//! The block's header hash was already checked against the pinned list entry
//! at receipt (the connection `Handler` hash check, design doc §2); `convert`
//! extends that pin from the header to the transaction bodies by reusing
//! [`zebra_consensus::merkle_root_validity`] verbatim, which also carries the
//! CVE-2012-2459 duplicate-txid check and the transaction branch-ID
//! consistency check.

use std::{
    future::Future,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
};

use futures::FutureExt;
use thiserror::Error;
use tower::{Service, ServiceExt};

use zebra_chain::{
    block::{self, Block},
    parameters::Network,
    serialization::ZcashDeserialize,
};
use zebra_consensus::{merkle_root_validity, spawn_fifo, BlockError};
use zebra_network::PeerSocketAddr;
use zebra_state::{self as zs, CheckpointVerifiedBlock};

use crate::BoxError;

/// Converts a fetched block into a [`CheckpointVerifiedBlock`] pinned by the
/// known-hash list, verifying everything the engine verifies per block
/// (design doc §2 check disposition table):
///
/// - the coinbase height matches the assigned `height`,
/// - the parent linkage matches `prev_expected` (`list[height - 1]`, or
///   [`GENESIS_PREVIOUS_BLOCK_HASH`] for the genesis block), and
/// - the transaction bodies match the pinned header's merkle root, via
///   [`zebra_consensus::merkle_root_validity`] — the header hash pins only
///   the header, so this check extends the pin to the bodies, and carries
///   the CVE-2012-2459 duplicate-txid and branch-ID consistency checks.
///
/// Returns the converted block and its serialized size in bytes.
///
/// This function is pure and CPU-heavy (transaction hashing and
/// `new_outputs` precomputation): callers run it on the rayon pool through
/// [`spawn_fifo`], so conversions parallelize across blocks.
///
/// `expected` (`list[height]`) was already matched against the block's
/// recomputed header hash at receipt, so it is re-pinned here without
/// re-hashing the header.
///
/// [`GENESIS_PREVIOUS_BLOCK_HASH`]: zebra_chain::parameters::GENESIS_PREVIOUS_BLOCK_HASH
pub fn convert(
    network: &Network,
    height: block::Height,
    expected: block::Hash,
    prev_expected: block::Hash,
    block: Arc<Block>,
) -> Result<CheckpointVerifiedBlock, ConvertError> {
    let coinbase_height = block.coinbase_height();
    if coinbase_height != Some(height) {
        return Err(ConvertError::WrongHeight {
            height,
            hash: expected,
            coinbase_height,
        });
    }

    if block.header.previous_block_hash != prev_expected {
        return Err(ConvertError::BrokenLink {
            height,
            hash: expected,
            prev_expected,
            found: block.header.previous_block_hash,
        });
    }

    // Transaction hashing and `new_outputs` precomputation: the expensive
    // part, parallel across blocks on the rayon pool. The coinbase height
    // was checked above, so the constructor's coinbase-height invariant
    // holds.
    let block_to_commit = CheckpointVerifiedBlock::with_hash(block, expected);

    match merkle_root_validity(
        network,
        &block_to_commit.block,
        &block_to_commit.transaction_hashes,
    ) {
        Ok(()) => Ok(block_to_commit),
        Err(BlockError::DuplicateTransaction) => Err(ConvertError::DuplicateTransaction {
            height,
            hash: expected,
            source_peer: None,
        }),
        // `BadMerkleRoot` or `WrongTransactionConsensusBranchId`: either way,
        // the delivered body does not belong to the pinned header.
        Err(error) => Err(ConvertError::BadMerkleRoot {
            height,
            hash: expected,
            source_peer: None,
            error,
        }),
    }
}

/// The block payload of an [`IbdBlock`]: either a block already deserialized
/// by zebra-network, or raw bytes promoted from the disk overflow tier
/// (design doc §4.5).
///
/// Raw bytes are **untrusted**: they are deserialized inside the service's
/// rayon closure (the engine loop never deserializes inline), and their
/// recomputed header hash is checked against the pinned `expected` hash
/// before [`convert`] runs — the receipt-time hash check that network blocks
/// already passed in the connection handler.
#[derive(Clone, Debug)]
pub enum BlockPayload {
    /// A block deserialized at receipt; its header hash already matched the
    /// pinned list entry in the connection handler.
    Block(Arc<Block>),

    /// Raw block bytes read back from the disk overflow tier, possibly
    /// corrupted or truncated while on disk.
    Raw(Vec<u8>),
}

impl From<Arc<Block>> for BlockPayload {
    fn from(block: Arc<Block>) -> Self {
        Self::Block(block)
    }
}

impl BlockPayload {
    /// Resolves the payload into a block whose header hash is `expected`.
    ///
    /// Raw cached bytes that do not deserialize, or whose recomputed header
    /// hash does not match the pinned `expected` hash, are corrupt cache
    /// entries (§4.5 exception (a)): the caller refetches the height.
    fn into_block(
        self,
        height: block::Height,
        expected: block::Hash,
    ) -> Result<Arc<Block>, ConvertError> {
        match self {
            Self::Block(block) => Ok(block),
            Self::Raw(bytes) => {
                let block = Block::zcash_deserialize(&bytes[..]).map_err(|error| {
                    ConvertError::CorruptCachedBytes {
                        height,
                        expected,
                        detail: format!("cached bytes do not deserialize: {error}"),
                    }
                })?;

                let found = block.hash();
                if found != expected {
                    return Err(ConvertError::CorruptCachedBytes {
                        height,
                        expected,
                        detail: format!("cached block hashes to {found:?}"),
                    });
                }

                Ok(Arc::new(block))
            }
        }
    }
}

/// A verify-stage failure from [`convert`], classified per the design doc
/// §4.3 failure semantics.
///
/// [`WrongHeight`](Self::WrongHeight) and [`BrokenLink`](Self::BrokenLink)
/// are **fatal list diagnostics**: the known-hash list (or its loader)
/// disagrees with a block whose header hash it pins, which refetching can
/// never cure. [`BadMerkleRoot`](Self::BadMerkleRoot) and
/// [`DuplicateTransaction`](Self::DuplicateTransaction) are
/// **peer-attributable**: the delivering peer sent a corrupt body for a real
/// header, so the engine reports the source peer and refetches from a
/// different one.
#[derive(Debug, Error)]
pub enum ConvertError {
    /// The block's coinbase height does not match the height the engine
    /// assigned from the known-hash list.
    ///
    /// Fatal list diagnostic: the coinbase height is pinned by the merkle
    /// root of the pinned header, so a mismatch means the list entry for
    /// `height` is wrong, not the block.
    #[error(
        "fatal known-hash list diagnostic: the list pins {hash:?} at {height:?}, \
         but the block's coinbase height is {coinbase_height:?}; \
         the list or its loader is broken"
    )]
    WrongHeight {
        /// The height assigned from the known-hash list.
        height: block::Height,
        /// The pinned hash for `height` (`list[height]`).
        hash: block::Hash,
        /// The coinbase height committed to by the block itself.
        coinbase_height: Option<block::Height>,
    },

    /// The block's previous block hash does not match the known-hash list
    /// entry below it.
    ///
    /// Fatal list diagnostic: the parent hash is part of the pinned header,
    /// so a mismatch means adjacent list entries disagree with each other.
    #[error(
        "fatal known-hash list diagnostic: the list pins {prev_expected:?} \
         below {hash:?} at {height:?}, but the block's previous block hash \
         is {found:?}; the list or its loader is broken"
    )]
    BrokenLink {
        /// The height assigned from the known-hash list.
        height: block::Height,
        /// The pinned hash for `height` (`list[height]`).
        hash: block::Hash,
        /// The pinned hash for the parent height (`list[height - 1]`).
        prev_expected: block::Hash,
        /// The previous block hash in the delivered block's header.
        found: block::Hash,
    },

    /// The transaction bodies do not belong to the pinned header: the
    /// merkle root does not match, or a transaction's consensus branch ID
    /// is inconsistent with the header's network upgrade.
    ///
    /// Peer-attributable: the header is real (its hash matched the list at
    /// receipt), so the delivering peer substituted the body.
    #[error(
        "block {hash:?} at {height:?} delivered by {source_peer:?} has a body \
         that does not belong to its pinned header: {error}"
    )]
    BadMerkleRoot {
        /// The height assigned from the known-hash list.
        height: block::Height,
        /// The pinned hash for `height` (`list[height]`).
        hash: block::Hash,
        /// The peer that delivered the block, when known.
        source_peer: Option<PeerSocketAddr>,
        /// The underlying check failure.
        #[source]
        error: BlockError,
    },

    /// The block contains duplicate transactions: a merkle malleability
    /// attempt (CVE-2012-2459) preserving the pinned root.
    ///
    /// Peer-attributable, like [`BadMerkleRoot`](Self::BadMerkleRoot).
    #[error(
        "block {hash:?} at {height:?} delivered by {source_peer:?} contains \
         duplicate transactions (merkle malleability, CVE-2012-2459)"
    )]
    DuplicateTransaction {
        /// The height assigned from the known-hash list.
        height: block::Height,
        /// The pinned hash for `height` (`list[height]`).
        hash: block::Hash,
        /// The peer that delivered the block, when known.
        source_peer: Option<PeerSocketAddr>,
    },

    /// Raw bytes from the disk overflow tier did not deserialize, or
    /// deserialized to a block whose header hash does not match the pinned
    /// list entry the bytes were stored under.
    ///
    /// A corrupt or torn cache entry (design doc §4.5 exception (a)): the
    /// engine discards the entry and refetches the height from the network.
    /// Not peer-attributable — the delivering peer's copy passed the
    /// receipt-time hash check before it was written to disk.
    #[error(
        "cached block bytes for {expected:?} at {height:?} are corrupt: \
         {detail}; discarding the cache entry and refetching"
    )]
    CorruptCachedBytes {
        /// The height assigned from the known-hash list.
        height: block::Height,
        /// The pinned hash the bytes were cached under (`list[height]`).
        expected: block::Hash,
        /// What was wrong with the bytes.
        detail: String,
    },

    /// The rayon pool dropped the conversion's response channel; Zebra is
    /// shutting down.
    #[error("the rayon threadpool dropped the conversion result; Zebra is likely shutting down")]
    RayonShutdown,
}

impl ConvertError {
    /// Attaches the delivering peer's address to peer-attributable failures.
    ///
    /// [`convert`] is pure and does not know the block's source, so
    /// [`VerifyAndCommit`] attaches it afterwards. List diagnostics and
    /// shutdown errors are returned unchanged: no peer is at fault for them.
    pub fn with_source_peer(mut self, peer: Option<PeerSocketAddr>) -> Self {
        match &mut self {
            Self::BadMerkleRoot { source_peer, .. }
            | Self::DuplicateTransaction { source_peer, .. } => *source_peer = peer,
            Self::WrongHeight { .. }
            | Self::BrokenLink { .. }
            | Self::CorruptCachedBytes { .. }
            | Self::RayonShutdown => {}
        }

        self
    }

    /// Returns true when this failure is attributable to the delivering
    /// peer: a corrupt body for a real (pinned) header.
    ///
    /// The engine reports the source peer for misbehavior and refetches the
    /// height from a different peer (design doc §4.3).
    pub fn is_peer_attributable(&self) -> bool {
        matches!(
            self,
            Self::BadMerkleRoot { .. } | Self::DuplicateTransaction { .. }
        )
    }

    /// Returns the delivering peer attached to a peer-attributable failure.
    pub fn source_peer(&self) -> Option<PeerSocketAddr> {
        match self {
            Self::BadMerkleRoot { source_peer, .. }
            | Self::DuplicateTransaction { source_peer, .. } => *source_peer,
            Self::WrongHeight { .. }
            | Self::BrokenLink { .. }
            | Self::CorruptCachedBytes { .. }
            | Self::RayonShutdown => None,
        }
    }

    /// Returns true when this failure is a fatal known-hash list diagnostic,
    /// which refetching can never cure (design doc §4.3).
    pub fn is_fatal_list_diagnostic(&self) -> bool {
        matches!(self, Self::WrongHeight { .. } | Self::BrokenLink { .. })
    }
}

/// An error from the [`VerifyAndCommit`] service.
///
/// The verify stage and the commit stage stay distinct so the engine can map
/// them per the design doc: verify failures classify by [`ConvertError`]
/// (§4.3); commit failures are state resets or shutdowns (§4.6) and are
/// never the delivering peer's fault.
#[derive(Debug, Error)]
pub enum VerifyAndCommitError {
    /// The block failed the verify stage; see [`ConvertError`] for the
    /// classification.
    #[error(transparent)]
    Verify(#[from] ConvertError),

    /// The state service accepted the request but failed to commit the
    /// block.
    #[error("the state failed to commit block {hash:?} at {height:?}: {error}")]
    Commit {
        /// The height of the block whose commit failed.
        height: block::Height,
        /// The pinned hash of the block whose commit failed.
        hash: block::Hash,
        /// The state service's error.
        #[source]
        error: BoxError,
    },

    /// The state service failed its readiness check, before any block was
    /// sent to it.
    #[error("the state service failed its readiness check: {0}")]
    StateUnready(#[source] BoxError),
}

/// A request to verify and commit one fetched block (design doc §4.3).
#[derive(Clone, Debug)]
pub struct IbdBlock {
    /// The height the engine assigned from the known-hash list: the block's
    /// index in the list.
    pub height: block::Height,

    /// The pinned hash for `height` (`list[height]`), already matched
    /// against the block's recomputed header hash at receipt.
    pub expected: block::Hash,

    /// The pinned hash for the parent height (`list[height - 1]`, or
    /// [`GENESIS_PREVIOUS_BLOCK_HASH`] for the genesis block).
    ///
    /// [`GENESIS_PREVIOUS_BLOCK_HASH`]: zebra_chain::parameters::GENESIS_PREVIOUS_BLOCK_HASH
    pub prev_expected: block::Hash,

    /// The fetched block, or its raw cached bytes when promoted from the
    /// disk overflow tier.
    pub block: BlockPayload,

    /// The peer that delivered the block, when known; attached to
    /// peer-attributable verify failures for misbehavior reporting.
    pub source: Option<PeerSocketAddr>,
}

/// The verify-and-commit tower service over the Buffer'd state service.
///
/// One call per block: verify on the rayon pool through [`convert`], then
/// commit through [`zs::Request::CommitCheckpointVerifiedBlock`]. The future
/// resolves with the committed hash only after the state accepts the block,
/// so unresolved futures are the engine's commit-pipeline cap unit (design
/// doc §4.4).
#[derive(Clone, Debug)]
pub struct VerifyAndCommit<ZS>
where
    ZS: Service<zs::Request, Response = zs::Response, Error = BoxError>
        + Send
        + Sync
        + Clone
        + 'static,
    ZS::Future: Send,
{
    /// The configured network, for the merkle check's branch-ID consistency.
    network: Network,

    /// The Buffer'd state service verified blocks are committed to.
    state: ZS,
}

impl<ZS> VerifyAndCommit<ZS>
where
    ZS: Service<zs::Request, Response = zs::Response, Error = BoxError>
        + Send
        + Sync
        + Clone
        + 'static,
    ZS::Future: Send,
{
    /// Returns a new verify-and-commit service over the Buffer'd `state`.
    pub fn new(network: Network, state: ZS) -> Self {
        Self { network, state }
    }
}

impl<ZS> Service<IbdBlock> for VerifyAndCommit<ZS>
where
    ZS: Service<zs::Request, Response = zs::Response, Error = BoxError>
        + Send
        + Sync
        + Clone
        + 'static,
    ZS::Future: Send,
{
    type Response = block::Hash;
    type Error = VerifyAndCommitError;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        // Delegate to the state Buffer. The engine's real backpressure is
        // its unresolved-future caps, not readiness (design doc §4.3).
        self.state
            .poll_ready(cx)
            .map_err(VerifyAndCommitError::StateUnready)
    }

    fn call(&mut self, request: IbdBlock) -> Self::Future {
        let IbdBlock {
            height,
            expected,
            prev_expected,
            block,
            source,
        } = request;

        // Clone before the async block (tower convention): the future must
        // not borrow from `self`. The clone's own readiness is established
        // by `oneshot` below.
        let network = self.network.clone();
        let state = self.state.clone();

        async move {
            // Payload resolution (deserializing raw cached bytes) runs inside
            // the rayon closure with `convert`: the calling task never does
            // CPU-heavy work inline (design doc §4.5).
            let block_to_commit = spawn_fifo(move || {
                let block = block.into_block(height, expected)?;
                convert(&network, height, expected, prev_expected, block)
            })
            .await
            .map_err(|_recv_error| ConvertError::RayonShutdown)?
            .map_err(|error| error.with_source_peer(source))?;

            match state
                .oneshot(zs::Request::CommitCheckpointVerifiedBlock(block_to_commit))
                .await
                .map_err(|error| VerifyAndCommitError::Commit {
                    height,
                    hash: expected,
                    error,
                })? {
                zs::Response::Committed(committed_hash) => {
                    assert_eq!(
                        committed_hash, expected,
                        "the state must commit the hash it was sent"
                    );
                    Ok(committed_hash)
                }
                _ => unreachable!("wrong response for CommitCheckpointVerifiedBlock"),
            }
        }
        .boxed()
    }
}
