use std::{collections::HashSet, fmt};

use zebra_chain::{
    block,
    transaction::{UnminedTx, UnminedTxId},
};

use super::super::types::Nonce;
use crate::{protocol::v2::types::ObjectHash, PeerSocketAddr};

#[cfg(any(test, feature = "proptest-impl"))]
use proptest_derive::Arbitrary;

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
    /// The second field is the address of the peer that pushed this `tx` to us:
    /// `Some(addr)` when a remote peer sent us the full transaction directly,
    /// and `None` when Zebra originates the push itself. Used by the mempool
    /// downloader to enforce a per-peer queue cap, mirroring
    /// [`Request::AdvertiseTransactionIds`]. See `GHSA-m9xx-8rcj-vmgp`.
    ///
    /// # Returns
    ///
    /// Returns [`Response::Nil`](super::Response::Nil).
    PushTransaction(UnminedTx, Option<PeerSocketAddr>),

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

    /// Request a contiguous chain of up to `count` blocks ending at
    /// `final_hash`, using the v2 protocol's `get-block-range` request.
    ///
    /// Every block is verified on arrival by hashing alone: the first
    /// delivered block must hash to `final_hash`, each subsequent block
    /// must be the parent of the previous one, and each block's
    /// transactions must match its header's merkle root, so a requester
    /// with a trusted anchor accepts no unverified bytes. The response may
    /// be truncated (a prefix of the range); the requester resumes from the
    /// last delivered block's parent hash. Legacy connections have no
    /// equivalent request, and answer with no blocks.
    ///
    /// # Returns
    ///
    /// Returns [`Response::Blocks`](super::Response::Blocks), in descending
    /// height order starting at `final_hash`, or with a single missing
    /// entry when the peer does not have the anchor block.
    BlockRange {
        /// The hash of the highest block in the requested range.
        final_hash: block::Hash,

        /// The maximum number of blocks requested.
        count: u64,

        /// The maximum total serialized size of the delivered blocks, in
        /// bytes. The first block is delivered regardless of this bound.
        max_bytes: u64,
    },

    /// Request best-chain block hashes and aggregated synchronization
    /// metadata at a height stride, serving the v2 protocol's `get-hashes`
    /// requests.
    ///
    /// This request is only sent to the local inbound service; it is never
    /// routed to a remote peer.
    ///
    /// # Returns
    ///
    /// Returns [`Response::SyncHashes`](super::Response::SyncHashes).
    SyncHashes {
        /// The height of the first requested entry.
        start_height: u32,

        /// The spacing between requested heights; never 0.
        stride: u32,

        /// The maximum number of entries requested.
        count: u32,
    },

    /// Request per-block note commitment tree roots and counts for a height
    /// range anchored at `final_hash`, serving the v2 protocol's
    /// `get-tree-roots` requests.
    ///
    /// This request is only sent to the local inbound service; it is never
    /// routed to a remote peer.
    ///
    /// # Returns
    ///
    /// Returns [`Response::TreeRoots`](super::Response::TreeRoots).
    TreeRoots {
        /// The height of the first requested entry.
        start_height: u32,

        /// The hash of the block at the highest requested height,
        /// `start_height + count − 1`, anchoring the request to a specific
        /// chain.
        final_hash: block::Hash,

        /// The number of entries requested.
        count: u32,
    },

    /// Request best-chain block hashes and aggregated synchronization
    /// metadata at a height stride from a remote v2 peer: the peer-routable
    /// form of [`Request::SyncHashes`], carried by the v2 protocol's
    /// `get-hashes` request stream.
    ///
    /// # Returns
    ///
    /// Returns [`Response::SyncHashes`](super::Response::SyncHashes).
    RemoteSyncHashes {
        /// The height of the first requested entry.
        start_height: u32,

        /// The spacing between requested heights; never 0.
        stride: u32,

        /// The maximum number of entries requested.
        count: u32,
    },

    /// Request per-block note commitment tree roots and counts for a height
    /// range anchored at `final_hash` from a remote v2 peer: the
    /// peer-routable form of [`Request::TreeRoots`], carried by the v2
    /// protocol's `get-tree-roots` request stream.
    ///
    /// # Returns
    ///
    /// Returns [`Response::TreeRoots`](super::Response::TreeRoots) with
    /// `Some` entries; a peer whose best chain does not contain the anchor
    /// refuses the request rather than answering for different blocks.
    RemoteTreeRoots {
        /// The height of the first requested entry.
        start_height: u32,

        /// The hash of the block at the highest requested height,
        /// `start_height + count − 1`, anchoring the request to a specific
        /// chain.
        final_hash: block::Hash,

        /// The number of entries requested.
        count: u32,
    },

    /// Request a byte range of a content-addressed synchronization artifact
    /// from a remote v2 peer, carried by the v2 protocol's `get-object`
    /// request stream.
    ///
    /// The caller verifies the delivered bytes against `hash`; the network
    /// layer does not interpret object contents. The whole exchange is
    /// bounded by the per-request timeout, so callers downloading large
    /// artifacts should request them in pieces of a few MiB rather than one
    /// maximal range.
    ///
    /// # Returns
    ///
    /// Returns [`Response::Object`](super::Response::Object).
    Object {
        /// The SHA-256 hash of the requested object.
        hash: ObjectHash,

        /// The byte offset into the object at which to start.
        offset: u64,

        /// The maximum number of bytes requested.
        length: u64,
    },

    /// Request a content-addressed synchronization artifact by its SHA-256
    /// hash, from the local node's pinned-constant lookup: a pinned
    /// known-hash chunk or the pinned spentness-hint artifact, serving the
    /// v2 protocol's `get-object` requests.
    ///
    /// This request is only sent to the local inbound service; it is never
    /// routed to a remote peer.
    ///
    /// # Returns
    ///
    /// Returns [`Response::Object`](super::Response::Object) with the whole
    /// artifact, or with a zero total size when the hash names no pinned
    /// artifact of this network or the artifact is not held.
    LocalObject {
        /// The SHA-256 hash of the requested object.
        hash: ObjectHash,
    },
}

/// What a peer must support to answer a [`Request`].
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum PeerCapability {
    /// Any connected peer can answer.
    Any,

    /// Only version 2 peers can answer: the request has no legacy protocol
    /// encoding.
    V2,
}

impl Request {
    /// Returns what a peer must support to answer this request.
    pub fn peer_capability(&self) -> PeerCapability {
        match self {
            // Bulk block streaming is a version 2 request stream, with no
            // legacy equivalent: the legacy locator walk is used instead.
            Request::BlockRange { .. } => PeerCapability::V2,

            // Answered by the local inbound service, never routed to a
            // peer: routing goes by capability, so a mis-routed request
            // still reaches a peer connection, which rejects it as
            // local-only.
            Request::SyncHashes { .. }
            | Request::TreeRoots { .. }
            | Request::LocalObject { .. } => PeerCapability::V2,

            // Synchronization and artifact requests exist only as v2
            // request streams; they have no legacy protocol encoding.
            Request::RemoteSyncHashes { .. }
            | Request::RemoteTreeRoots { .. }
            | Request::Object { .. } => PeerCapability::V2,

            _ => PeerCapability::Any,
        }
    }
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

            Request::PushTransaction(..) => "PushTransaction".to_string(),
            Request::AdvertiseTransactionIds(ids, _) => {
                format!("AdvertiseTransactionIds({})", ids.len())
            }

            Request::AdvertiseBlock(_, _) => "AdvertiseBlock".to_string(),
            Request::AdvertiseBlockToAll(_) => "AdvertiseBlockToAll".to_string(),
            Request::MempoolTransactionIds => "MempoolTransactionIds".to_string(),
            Request::BlockRange { count, .. } => format!("BlockRange({count})"),
            Request::SyncHashes { count, .. } => format!("SyncHashes({count})"),
            Request::TreeRoots { count, .. } => format!("TreeRoots({count})"),
            Request::RemoteSyncHashes { count, .. } => format!("RemoteSyncHashes({count})"),
            Request::RemoteTreeRoots { count, .. } => format!("RemoteTreeRoots({count})"),
            Request::Object { length, .. } => format!("Object({length})"),
            Request::LocalObject { .. } => "LocalObject".to_string(),
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

            Request::PushTransaction(..) => "PushTransaction",
            Request::AdvertiseTransactionIds(_, _) => "AdvertiseTransactionIds",

            Request::AdvertiseBlock(_, _) | Request::AdvertiseBlockToAll(_) => "AdvertiseBlock",
            Request::MempoolTransactionIds => "MempoolTransactionIds",
            Request::BlockRange { .. } => "BlockRange",
            Request::SyncHashes { .. } => "SyncHashes",
            Request::TreeRoots { .. } => "TreeRoots",
            Request::RemoteSyncHashes { .. } => "RemoteSyncHashes",
            Request::RemoteTreeRoots { .. } => "RemoteTreeRoots",
            Request::Object { .. } => "Object",
            Request::LocalObject { .. } => "LocalObject",
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
