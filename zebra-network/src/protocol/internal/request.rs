use std::{collections::HashSet, sync::Arc};

use zebra_chain::{
    block,
    transaction::{self, Transaction},
};

use super::super::types::Nonce;

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
#[derive(Clone, Debug)]
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
    /// # Returns
    ///
    /// Returns [`Response::Blocks`](super::Response::Blocks).
    BlocksByHash(HashSet<block::Hash>),

    /// Request transactions by hash.
    ///
    /// This uses a `HashSet` for the same reason as [`Request::BlocksByHash`].
    ///
    /// If this requests a recently-advertised transaction, the peer set will
    /// make a best-effort attempt to route the request to a peer that advertised
    /// the transaction. This routing is only used for request sets of size 1.
    /// Otherwise, it is routed using the normal load-balancing strategy.
    ///
    /// # Returns
    ///
    /// Returns [`Response::Transactions`](super::Response::Transactions).
    TransactionsByHash(HashSet<transaction::Hash>),

    /// Request block hashes of subsequent blocks in the chain, given hashes of
    /// known blocks.
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
        known_blocks: Vec<block::Hash>,
        /// Optionally, the last block hash to request.
        stop: Option<block::Hash>,
    },

    /// Request headers of subsequent blocks in the chain, given hashes of
    /// known blocks.
    ///
    /// # Returns
    ///
    /// Returns
    /// [`Response::BlockHeaders`](super::Response::BlockHeaders).
    FindHeaders {
        /// Hashes of known blocks, ordered from highest height to lowest height.
        known_blocks: Vec<block::Hash>,
        /// Optionally, the last header to request.
        stop: Option<block::Hash>,
    },

    /// Push a transaction to a remote peer, without advertising it to them first.
    ///
    /// This is implemented by sending an unsolicited `tx` message.
    ///
    /// # Returns
    ///
    /// Returns [`Response::Nil`](super::Response::Nil).
    PushTransaction(Arc<Transaction>),

    /// Advertise a set of transactions to all peers.
    ///
    /// This is intended to be used in Zebra with a single transaction at a time
    /// (set of size 1), but multiple transactions are permitted because this is
    /// how we interpret advertisements from zcashd, which sometimes advertises
    /// multiple transactions at once.
    ///
    /// This is implemented by sending an `inv` message containing the
    /// transaction hash, allowing the remote peer to choose whether to download
    /// it. Remote peers who choose to download the transaction will generate a
    /// [`Request::TransactionsByHash`] against the "inbound" service passed to
    /// [`zebra_network::init`].
    ///
    /// The peer set routes this request specially, sending it to *every*
    /// available peer.
    ///
    /// # Returns
    ///
    /// Returns [`Response::Nil`](super::Response::Nil).
    AdvertiseTransactions(HashSet<transaction::Hash>),

    /// Advertise a block to all peers.
    ///
    /// This is implemented by sending an `inv` message containing the
    /// block hash, allowing the remote peer to choose whether to download
    /// it. Remote peers who choose to download the block will generate a
    /// [`Request::BlocksByHash`] against the "inbound" service passed to
    /// [`zebra_network::init`].
    ///
    /// The peer set routes this request specially, sending it to *every*
    /// available peer.
    ///
    /// # Returns
    ///
    /// Returns [`Response::Nil`](super::Response::Nil).
    AdvertiseBlock(block::Hash),

    /// Request the contents of this node's mempool.
    ///
    /// # Returns
    ///
    /// Returns [`Response::TransactionHashes`](super::Response::TransactionHashes).
    MempoolTransactions,
}
