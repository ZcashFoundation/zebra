use std::{collections::HashSet, fmt};

use zebra_chain::{
    block,
    transaction::{UnminedTx, UnminedTxId},
};

use super::super::types::Nonce;
use crate::PeerSocketAddr;

#[cfg(any(test, feature = "proptest-impl"))]
use proptest_derive::Arbitrary;

/// A shielded pool selector for note-commitment-tree requests.
///
/// Used by [`Request::NoteCommitmentTree`] to pick which of the two shielded
/// note commitment trees (Sapling or Orchard) is being requested for a height.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
#[cfg_attr(any(test, feature = "proptest-impl"), derive(Arbitrary))]
pub enum ShieldedPool {
    /// The Sapling note commitment tree.
    Sapling,

    /// The Orchard note commitment tree.
    Orchard,
}

/// A network request, represented in internal format.
///
/// The network layer aims to abstract away the details of the Bitcoin wire
/// protocol into a clear request/response API. Each [`Request`] documents the
/// possible [`Response`s](super::Response) it can generate; it is fine (and
/// recommended!) to match on the expected responses and treat the others as
/// `unreachable!()`, since their return indicates a bug in the network code.
///
/// # Cancellations
///
/// The peer set handles cancelled requests (i.e., requests where the future
/// returned by `Service::call` is dropped before it resolves) on a best-effort
/// basis. Requests are routed to a particular peer connection, and then
/// translated into Zcash protocol messages and sent over the network. If a
/// request is cancelled after it is submitted but before it is processed by a
/// peer connection, no messages will be sent. Otherwise, if it is cancelled
/// while waiting for a response, the peer connection resets its state and makes
/// a best-effort attempt to ignore any messages responsive to the cancelled
/// request, subject to limitations in the underlying Zcash protocol.
#[derive(Clone, Debug, Eq, PartialEq)]
#[cfg_attr(any(test, feature = "proptest-impl"), derive(Arbitrary))]
pub enum Request {
    /// Requests additional peers from the server.
    ///
    /// # Response
    ///
    /// Returns [`Response::Peers`](super::Response::Peers).
    Peers,

    /// Heartbeats triggered on peer connection start.
    ///
    /// This is included as a bit of a hack, it should only be used
    /// internally for connection management. You should not expect to
    /// be firing or handling `Ping` requests or `Pong` responses.
    #[doc(hidden)]
    Ping(Nonce),

    /// Request block data by block hashes.
    ///
    /// This uses a `HashSet` rather than a `Vec` for two reasons. First, it
    /// automatically deduplicates the requested blocks. Second, the internal
    /// protocol translator needs to maintain a `HashSet` anyways, in order to
    /// keep track of which requested blocks have been received and when the
    /// request is ready. Rather than force the internals to always convert into
    /// a `HashSet`, we require the caller to pass one, so that if the caller
    /// didn't start with a `Vec` but with, e.g., an iterator, they can collect
    /// directly into a `HashSet` and save work.
    ///
    /// If this requests a recently-advertised block, the peer set will make a
    /// best-effort attempt to route the request to a peer that advertised the
    /// block. This routing is only used for request sets of size 1.
    /// Otherwise, it is routed using the normal load-balancing strategy.
    ///
    /// The list contains zero or more block hashes.
    ///
    /// # Returns
    ///
    /// Returns [`Response::Blocks`](super::Response::Blocks).
    BlocksByHash(HashSet<block::Hash>),

    /// Request transactions by their unmined transaction ID.
    ///
    /// v4 transactions use a legacy transaction ID, and
    /// v5 transactions use a witnessed transaction ID.
    ///
    /// This uses a `HashSet` for the same reason as [`Request::BlocksByHash`].
    ///
    /// If this requests a recently-advertised transaction, the peer set will
    /// make a best-effort attempt to route the request to a peer that advertised
    /// the transaction. This routing is only used for request sets of size 1.
    /// Otherwise, it is routed using the normal load-balancing strategy.
    ///
    /// The list contains zero or more unmined transaction IDs.
    ///
    /// # Returns
    ///
    /// Returns [`Response::Transactions`](super::Response::Transactions).
    TransactionsById(HashSet<UnminedTxId>),

    /// Request block hashes of subsequent blocks in the chain, given hashes of
    /// known blocks.
    ///
    /// The known blocks list contains zero or more block hashes.
    ///
    /// # Returns
    ///
    /// Returns
    /// [`Response::BlockHashes`](super::Response::BlockHashes).
    ///
    /// # Warning
    ///
    /// This is implemented by sending a `getblocks` message. Bitcoin nodes
    /// respond to `getblocks` with an `inv` message containing a list of the
    /// subsequent blocks. However, Bitcoin nodes *also* send `inv` messages
    /// unsolicited in order to gossip new blocks to their peers. These gossip
    /// messages can race with the response to a `getblocks` request, and there
    /// is no way for the network layer to distinguish them. For this reason, the
    /// response may occasionally contain a single hash of a new chain tip rather
    /// than a list of hashes of subsequent blocks. We believe that unsolicited
    /// `inv` messages will always have exactly one block hash.
    FindBlocks {
        /// Hashes of known blocks, ordered from highest height to lowest height.
        //
        // TODO: make this into an IndexMap - an ordered unique list of hashes (#2244)
        known_blocks: Vec<block::Hash>,
        /// Optionally, the last block hash to request.
        stop: Option<block::Hash>,
    },

    /// Request headers of subsequent blocks in the chain, given hashes of
    /// known blocks.
    ///
    /// The known blocks list contains zero or more block hashes.
    ///
    /// # Returns
    ///
    /// Returns
    /// [`Response::BlockHeaders`](super::Response::BlockHeaders).
    FindHeaders {
        /// Hashes of known blocks, ordered from highest height to lowest height.
        //
        // TODO: make this into an IndexMap - an ordered unique list of hashes (#2244)
        known_blocks: Vec<block::Hash>,
        /// Optionally, the last header to request.
        stop: Option<block::Hash>,
    },

    /// Push an unmined transaction to a remote peer, without advertising it to them first.
    ///
    /// This is implemented by sending an unsolicited `tx` message.
    ///
    /// # Returns
    ///
    /// Returns [`Response::Nil`](super::Response::Nil).
    PushTransaction(UnminedTx),

    /// Advertise a set of unmined transactions to all peers.
    ///
    /// Both Zebra and zcashd sometimes advertise multiple transactions at once.
    ///
    /// This is implemented by sending an `inv` message containing the unmined
    /// transaction IDs, allowing the remote peer to choose whether to download
    /// them. Remote peers who choose to download the transaction will generate a
    /// [`Request::TransactionsById`] against the "inbound" service passed to
    /// [`init`](crate::init).
    ///
    /// v4 transactions use a legacy transaction ID, and
    /// v5 transactions use a witnessed transaction ID.
    ///
    /// The list contains zero or more transaction IDs.
    ///
    /// The peer set routes this request specially, sending it to *half of*
    /// the available peers.
    ///
    /// The second field is the address of the peer that sent us this `inv`:
    /// `Some(addr)` when the advertisement was relayed from a remote peer,
    /// and `None` when Zebra originates the advertisement itself (e.g. the
    /// mempool gossip task). Used by the mempool downloader to enforce a
    /// per-peer queue cap. See `GHSA-4fc2-h7jh-287c`.
    ///
    /// # Returns
    ///
    /// Returns [`Response::Nil`](super::Response::Nil).
    AdvertiseTransactionIds(HashSet<UnminedTxId>, Option<PeerSocketAddr>),

    /// Advertise a block to all peers.
    ///
    /// This is implemented by sending an `inv` message containing the
    /// block hash, allowing the remote peer to choose whether to download
    /// it. Remote peers who choose to download the block will generate a
    /// [`Request::BlocksByHash`] against the "inbound" service passed to
    /// [`init`](crate::init).
    ///
    /// The peer set routes this request specially, sending it to *a fraction of*
    /// the available peers. See [`number_of_peers_to_broadcast()`](crate::PeerSet::number_of_peers_to_broadcast)
    /// for more details.
    ///
    /// The second field is the address of the peer that sent us this `inv`:
    /// `Some(addr)` when the advertisement was relayed from a remote peer,
    /// and `None` when Zebra originates the advertisement itself (for
    /// example from the sync gossip task). Consumers use the address to
    /// apply per-peer policies such as the inbound download per-IP cap.
    ///
    /// # Returns
    ///
    /// Returns [`Response::Nil`](super::Response::Nil).
    AdvertiseBlock(block::Hash, Option<PeerSocketAddr>),

    /// Advertise a block to all ready peers. This is equivalent to
    /// [`Request::AdvertiseBlock`] except that the peer set will route
    /// this request to all available ready peers. Used by the gossip task
    /// to broadcast mined blocks to all ready peers.
    AdvertiseBlockToAll(block::Hash),

    /// Request the contents of this node's mempool.
    ///
    /// # Returns
    ///
    /// Returns [`Response::TransactionIds`](super::Response::TransactionIds).
    MempoolTransactionIds,

    /// Request a byte range of a known-hash chunk's deterministic bytes.
    ///
    /// A chunk is a deterministic, content-addressed encoding of a span of
    /// blocks (block hashes, size hints, and shielded tree roots). The peer
    /// regenerates chunk `index` from its finalized state, so every honest node
    /// produces byte-identical output that hashes to the pinned
    /// `chunk_hashes[index]` constant. See `docs/design/p2p-snapshot-distribution.md`.
    ///
    /// A full v2 chunk (~4.72 MiB) exceeds `MAX_PROTOCOL_MESSAGE_LEN` (2 MiB), so
    /// chunks are transferred in `≤ 1 MiB` ranges like the snapshot sets; the
    /// requester reassembles the full chunk from its ranges and verifies its
    /// SHA-256 against `chunk_hashes[index]`.
    ///
    /// # Returns
    ///
    /// Returns [`Response::SnapshotRange`](super::Response::SnapshotRange) with
    /// the requested bytes, or [`Response::NotFound`](super::Response::NotFound)
    /// if the chunk index is unknown/above the peer's tip, the offset is past the
    /// chunk end, or the length exceeds the per-request limit.
    KnownHashChunkRange {
        /// The chunk index.
        index: u32,
        /// The byte offset into the deterministic chunk bytes.
        offset: u64,
        /// The number of bytes to return, bounded by the per-request limit.
        len: u32,
    },

    /// Request the serialized note commitment tree of a shielded pool, as of a
    /// given block height.
    ///
    /// The serialization is deterministic so the requester's recomputed
    /// `.root()` matches the root recorded in the relevant known-hash chunk.
    ///
    /// # Returns
    ///
    /// Returns [`Response::NoteCommitmentTree`](super::Response::NoteCommitmentTree)
    /// if the tree is available, or [`Response::NotFound`](super::Response::NotFound)
    /// if the height is unknown or above the peer's tip.
    NoteCommitmentTree {
        /// The shielded pool whose tree is requested.
        pool: ShieldedPool,
        /// The block height at which to snapshot the tree.
        height: block::Height,
    },

    /// Request a byte range of the unspent transparent output set at the max
    /// checkpoint height.
    ///
    /// The set is the sorted concatenation of fixed-size `OutputLocation`s. It
    /// is served ranged into sub-chunks bounded under the 2 MiB protocol frame;
    /// the assembled set is verified against a pinned SHA-256 constant.
    ///
    /// # Returns
    ///
    /// Returns [`Response::SnapshotRange`](super::Response::SnapshotRange) with
    /// the requested bytes, or [`Response::NotFound`](super::Response::NotFound)
    /// if the range is out of bounds or the length exceeds the per-request limit.
    UnspentOutputs {
        /// The byte offset into the unspent-output set.
        offset: u64,
        /// The number of bytes to return, bounded by the per-request limit.
        len: u32,
    },

    /// Request a byte range of the address-balance set at the max checkpoint
    /// height.
    ///
    /// The set is the sorted concatenation of fixed-size
    /// `(transparent::Address, AddressBalanceLocation)` records. It is served
    /// and verified like the unspent-output set.
    ///
    /// # Returns
    ///
    /// Returns [`Response::SnapshotRange`](super::Response::SnapshotRange) with
    /// the requested bytes, or [`Response::NotFound`](super::Response::NotFound)
    /// if the range is out of bounds or the length exceeds the per-request limit.
    AddressBalances {
        /// The byte offset into the address-balance set.
        offset: u64,
        /// The number of bytes to return, bounded by the per-request limit.
        len: u32,
    },
}

impl fmt::Display for Request {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str(&match self {
            Request::Peers => "Peers".to_string(),
            Request::Ping(_) => "Ping".to_string(),

            Request::BlocksByHash(hashes) => {
                format!("BlocksByHash({})", hashes.len())
            }
            Request::TransactionsById(ids) => format!("TransactionsById({})", ids.len()),

            Request::FindBlocks { known_blocks, stop } => format!(
                "FindBlocks {{ known_blocks: {}, stop: {} }}",
                known_blocks.len(),
                if stop.is_some() { "Some" } else { "None" },
            ),
            Request::FindHeaders { known_blocks, stop } => format!(
                "FindHeaders {{ known_blocks: {}, stop: {} }}",
                known_blocks.len(),
                if stop.is_some() { "Some" } else { "None" },
            ),

            Request::PushTransaction(_) => "PushTransaction".to_string(),
            Request::AdvertiseTransactionIds(ids, _) => {
                format!("AdvertiseTransactionIds({})", ids.len())
            }

            Request::AdvertiseBlock(_, _) => "AdvertiseBlock".to_string(),
            Request::AdvertiseBlockToAll(_) => "AdvertiseBlockToAll".to_string(),
            Request::MempoolTransactionIds => "MempoolTransactionIds".to_string(),

            Request::KnownHashChunkRange { index, offset, len } => {
                format!("KnownHashChunkRange {{ index: {index}, offset: {offset}, len: {len} }}")
            }
            Request::NoteCommitmentTree { pool, height } => {
                format!(
                    "NoteCommitmentTree {{ pool: {pool:?}, height: {} }}",
                    height.0
                )
            }
            Request::UnspentOutputs { offset, len } => {
                format!("UnspentOutputs {{ offset: {offset}, len: {len} }}")
            }
            Request::AddressBalances { offset, len } => {
                format!("AddressBalances {{ offset: {offset}, len: {len} }}")
            }
        })
    }
}

impl Request {
    /// Returns the Zebra internal request type as a string.
    pub fn command(&self) -> &'static str {
        match self {
            Request::Peers => "Peers",
            Request::Ping(_) => "Ping",

            Request::BlocksByHash(_) => "BlocksByHash",
            Request::TransactionsById(_) => "TransactionsById",

            Request::FindBlocks { .. } => "FindBlocks",
            Request::FindHeaders { .. } => "FindHeaders",

            Request::PushTransaction(_) => "PushTransaction",
            Request::AdvertiseTransactionIds(_, _) => "AdvertiseTransactionIds",

            Request::AdvertiseBlock(_, _) | Request::AdvertiseBlockToAll(_) => "AdvertiseBlock",
            Request::MempoolTransactionIds => "MempoolTransactionIds",

            Request::KnownHashChunkRange { .. } => "KnownHashChunkRange",
            Request::NoteCommitmentTree { .. } => "NoteCommitmentTree",
            Request::UnspentOutputs { .. } => "UnspentOutputs",
            Request::AddressBalances { .. } => "AddressBalances",
        }
    }

    /// Returns true if the request is for block or transaction inventory downloads.
    pub fn is_inventory_download(&self) -> bool {
        matches!(
            self,
            Request::BlocksByHash(_) | Request::TransactionsById(_)
        )
    }

    /// Returns the block hash inventory downloads from the request, if any.
    pub fn block_hash_inventory(&self) -> HashSet<block::Hash> {
        if let Request::BlocksByHash(block_hashes) = self {
            block_hashes.clone()
        } else {
            HashSet::new()
        }
    }

    /// Returns the transaction ID inventory downloads from the request, if any.
    pub fn transaction_id_inventory(&self) -> HashSet<UnminedTxId> {
        if let Request::TransactionsById(transaction_ids) = self {
            transaction_ids.clone()
        } else {
            HashSet::new()
        }
    }
}
